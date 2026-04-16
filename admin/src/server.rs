//! [`AdminServer`] implementation.

use std::path::Path;

use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName,
    tokio::Listener,
    traits::{StreamCommon, tokio::Listener as _},
};
#[cfg(unix)]
use nix::unistd::Uid;
use novelnote_database::Database;
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, instrument, trace};
use tracing_error::TracedError;

use crate::{Connection, ConnectionError};

/// Admin server for NovelNote. Listens on a local socket.
#[derive(Debug)]
pub struct AdminServer {
    /// Admin socket listener.
    listener: Listener,

    /// UID of the current process.
    #[cfg(unix)]
    current_uid: Uid,

    /// NovelNote database handle.
    database: Database,
}

impl AdminServer {
    /// Create an admin server, binding a listener to `path`.
    ///
    /// `path` is used for the location of a Unix domain socket (Linux and macOS) or a named pipe
    /// namespace (Windows). On Windows, the path must start with `\\.\pipe\`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `path` is invalid or cannot be bound to.
    ///
    /// # Panics
    ///
    /// Panics if not called within a Tokio runtime with IO enabled.
    #[instrument(level = "debug", skip_all, fields(path = %path.display()), err)]
    pub fn bind(path: &Path, database: Database) -> Result<Self, TracedError<ConnectionError>> {
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
        Ok(stream.into())
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
            let connection = match result {
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
            let handle_connection = async {
                if let Err(error) = handle_connection(connection, database).await {
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
/// Returns an error if the connection cannot be read from or written to.
#[instrument(level = "trace", skip_all)]
async fn handle_connection(
    mut connection: Connection,
    _database: Database,
) -> Result<(), HandleConnectionError> {
    let bytes = connection
        .read()
        .await
        .map_err(HandleConnectionError::Read)?;

    // TODO: actually handle messages, for now just echo back.
    connection
        .write(bytes.freeze())
        .await
        .map_err(HandleConnectionError::Write)?;

    Ok(())
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

    /// Error while writing to the [`Connection`].
    #[error("error writing to connection: {0}")]
    Write(std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::{error::Error, time::Duration};

    use bytes::Bytes;
    #[cfg(unix)]
    use tempfile::TempDir;
    use tokio_util::time::FutureExt;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(3);

    #[test_log::test(tokio::test)]
    async fn echo() -> Result<(), Box<dyn Error>> {
        #[cfg(unix)]
        let (temp_dir, path) = {
            let temp_dir = TempDir::new()?;
            let path = temp_dir.path().join("test_echo.sock");
            (temp_dir, path)
        };

        #[cfg(windows)]
        let path = std::path::PathBuf::from(r"\\.\pipe\NoveNote_Admin_Test_Echo");

        let server = AdminServer::bind(&path, Database::open_in_memory(1).await?)?;
        let cancellation_token = CancellationToken::new();
        let child_token = cancellation_token.child_token();
        let server = tokio::spawn(async move { server.run(&child_token).await });

        let mut connection = Connection::new(&path).timeout(TIMEOUT).await??;

        let message = Bytes::from("Hello World!");
        trace!("writing message");
        connection.write(message.clone()).timeout(TIMEOUT).await??;
        trace!("reading response");
        let response = connection.read().timeout(TIMEOUT).await??;
        assert_eq!(message, response);

        cancellation_token.cancel();
        server.await?;
        #[cfg(unix)]
        temp_dir.close()?;

        Ok(())
    }
}
