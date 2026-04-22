//! [`AdminServer`] implementation.

use std::{path::Path, time::Duration};

use bytes::Bytes;
use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName,
    tokio::Listener,
    traits::{StreamCommon, tokio::Listener as _},
};
#[cfg(unix)]
use nix::unistd::Uid;
use novelnote_database::Database;
use rkyv::{rancor, util::AlignedVec};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, instrument, trace};
use tracing_error::TracedError;

use crate::{
    ArchivedMessage, Connection, ConnectionError, DatabaseClosed, DeserializeError, ResponseError,
};

/// Admin server for NovelNote. Listens on a local socket.
#[derive(Debug)]
pub struct AdminServer {
    /// Admin socket listener.
    listener: Listener,

    /// UID of the current process.
    #[cfg(unix)]
    current_uid: Uid,

    /// How long to wait when sending or receiving data before timing out.
    timeout: Duration,

    /// NovelNote database handle.
    database: Database,
}

impl AdminServer {
    /// Create an admin server, binding a listener to `path`.
    ///
    /// `path` is used for the location of a Unix domain socket (Linux and macOS) or a named pipe
    /// namespace (Windows). On Windows, the path must start with `\\.\pipe\`.
    ///
    /// Connections will time out after `timeout` expires.
    ///
    /// # Errors
    ///
    /// Returns an error if the `path` is invalid or cannot be bound to.
    ///
    /// # Panics
    ///
    /// Panics if not called within a Tokio runtime with IO enabled.
    #[instrument(level = "debug", skip_all, fields(path = %path.display()), err)]
    pub fn bind(
        path: &Path,
        timeout: Duration,
        database: Database,
    ) -> Result<Self, TracedError<ConnectionError>> {
        let name = path.to_fs_name::<GenericFilePath>().map_err(|source| {
            ConnectionError::InvalidPath {
                source,
                path: path.to_owned(),
            }
        })?;

        let listener_options = ListenerOptions::new().name(name).try_overwrite(true);

        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        let listener_options = {
            use interprocess::os::unix::local_socket::ListenerOptionsExt;
            listener_options.mode(0o0750)
        };

        let listener = listener_options
            .create_tokio()
            .map_err(|source| ConnectionError::Bind {
                source,
                path: path.to_owned(),
            })?;

        debug!("admin server bound to local socket");
        Ok(Self {
            listener,
            #[cfg(unix)]
            current_uid: Uid::effective(),
            timeout,
            database,
        })
    }

    /// Listen for an incoming connection to the socket.
    ///
    /// # Errors
    ///
    /// Returns an error if there an error accepting the connection, it's credentials could not be
    /// determined, or it's UID is not root or does not match the current process's UID.
    #[instrument(level = "trace", skip(self))]
    async fn accept(&self) -> Result<Connection, AcceptError> {
        let stream = self.listener.accept().await.map_err(AcceptError::Accept)?;

        let peer_creds = stream.peer_creds().map_err(AcceptError::PeerCreds)?;

        #[cfg(unix)]
        {
            let incoming_uid = Uid::from_raw(peer_creds.euid().ok_or(AcceptError::UidUnknown)?);
            if !incoming_uid.is_root() && incoming_uid != self.current_uid {
                return Err(AcceptError::Unauthorized(incoming_uid));
            }
        }

        trace!(
            incoming_pid = peer_creds.pid(),
            "accepted connection on admin socket"
        );
        Ok(Connection::from_stream(stream, self.timeout))
    }

    /// Start the admin server.
    ///
    /// The future will not complete until the `cancellation_token` is
    /// [cancelled](CancellationToken::cancel()) and all connections have responded to their
    /// requests.
    #[instrument(level = "debug", skip_all)]
    pub async fn run(&self, cancellation_token: &CancellationToken) {
        let (closed_sender, closed_receiver) = watch::channel(());

        info!("admin server listening on local socket");

        loop {
            let result = tokio::select! {
                result = self.accept() => result,
                () = cancellation_token.cancelled() => {
                    trace!(
                        "graceful shutdown signal received, not accepting new admin connections"
                    );
                    break;
                }
            };
            let mut connection = match result {
                Ok(connection) => connection,
                Err(error) => {
                    error!(
                        ?error,
                        "error accepting connection on admin socket: {error}"
                    );
                    debug_assert!(false, "admin socket connection error");
                    continue;
                }
            };

            let database = self.database.clone();
            let closed_receiver = closed_receiver.clone();
            let handle_connection = async move {
                if let Err(error) = handle_connection(&mut connection, &database).await {
                    error!(?error, "error handling admin socket connection: {error}");
                    debug_assert!(false, "admin socket communication error");
                }
                drop(closed_receiver);
            };
            tokio::spawn(handle_connection.in_current_span());
        }

        drop(closed_receiver);

        trace!(
            "waiting for {} admin task(s) to complete",
            closed_sender.receiver_count()
        );
        closed_sender.closed().await;
    }
}

/// Handle an incoming connection to the admin socket.
///
/// # Errors
///
/// Returns an error if the connection cannot be read from or written to, or the client's
/// [`Message`](crate::Message) could not be deserialized.
#[instrument(level = "trace", skip_all)]
async fn handle_connection(
    connection: &mut Connection,
    database: &Database,
) -> Result<(), HandleConnectionError> {
    let bytes = connection
        .read()
        .await
        .map_err(HandleConnectionError::Read)?;

    let message = rkyv::access::<ArchivedMessage, _>(&bytes)
        .map_err(HandleConnectionError::from_deserialize)?;
    trace!(?message, "message received from admin socket");

    let response = Bytes::from_owner(message.response(database).await);

    connection
        .write(response)
        .await
        .map_err(HandleConnectionError::Write)?;

    Ok(())
}

impl ArchivedMessage {
    /// Based on the received [`Message`](crate::Message), determine the response and serialize it.
    async fn response(&self, database: &Database) -> AlignedVec {
        let serialized_result: Result<_, rancor::Error> = match self {
            Self::HealthCheck => {
                let result = if database.is_open() {
                    Ok(())
                } else {
                    Err(DatabaseClosed)
                };
                rkyv::to_bytes(&result)
            }
            Self::Backup { path } => {
                let result = database
                    .backup(path.as_str().to_owned())
                    .await
                    .map_err(|error| {
                        error!(?error, %path, "error during database backup");
                        ResponseError::from(error)
                    });
                rkyv::to_bytes(&result)
            }
        };
        serialized_result.expect("response cannot fail to serialize into bytes")
    }
}

/// Possible errors when [`accepting`](AdminServer::accept()) new connections.
//
// Usually, the source error message is not exposed in the message for the wrapping error, but in
// this case the error is just logged and not propagated.
#[derive(Error, Debug)]
enum AcceptError {
    /// Error accepting new connection.
    #[error("error accepting new connection: {0}")]
    Accept(std::io::Error),

    /// Error getting [`PeerCreds`](interprocess::local_socket::PeerCreds) of incoming connection.
    #[error("error getting credentials of incoming connection: {0}")]
    PeerCreds(std::io::Error),

    /// UID of the incoming connection is unknown.
    #[cfg(unix)]
    #[error("could not determine UID of incoming connection")]
    UidUnknown,

    /// UID of the incoming connection is not root or the UID of the current process.
    #[cfg(unix)]
    #[error("uid {0} is not authorized to access the admin socket")]
    Unauthorized(Uid),
}

/// Error returned when [`handle_connection()`] fails.
#[derive(Error, Debug)]
enum HandleConnectionError {
    /// Error while reading from the [`Connection`].
    #[error("error reading from connection: {0}")]
    Read(std::io::Error),

    /// Error deserializing [`Message`](crate::Message) from client.
    #[error("error deserializing message")]
    Deserialize(#[from] DeserializeError),

    /// Error while writing to the [`Connection`].
    #[error("error writing to connection: {0}")]
    Write(std::io::Error),
}

impl HandleConnectionError {
    /// Error while deserializing.
    const fn from_deserialize(error: rancor::Error) -> Self {
        Self::Deserialize(DeserializeError(error))
    }
}
