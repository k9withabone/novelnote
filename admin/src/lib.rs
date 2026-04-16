//! `novelnote_admin` provides [`AdminServer`] for communication over the admin
//! socket for NovelNote, a self-hosted book tracker.
//!
//! The admin socket's interface is not considered to be a part of this crate's public API.

mod client;
mod server;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::SinkExt;
use interprocess::local_socket::{
    GenericFilePath, ToFsName, tokio::Stream, traits::tokio::Stream as _,
};
use rkyv::{Archive, Deserialize, Serialize, rancor};
use thiserror::Error;
use tokio_stream::StreamExt;
use tokio_util::{
    codec::{Framed, LengthDelimitedCodec},
    time::FutureExt,
};

pub use self::{
    client::{AdminClient, CommunicationError, HealthCheckError, ReceiveError, SendError},
    server::AdminServer,
};

/// Connection between the [`AdminServer`] and client, using a local socket.
#[derive(Debug)]
struct Connection {
    /// Local socket byte stream, framed by a length prefix.
    stream: Framed<Stream, LengthDelimitedCodec>,

    /// How long to wait when sending or receiving data before timing out.
    timeout: Duration,
}

impl Connection {
    /// Connect to an [`AdminServer`] bound to `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `path` is invalid or cannot be bound to.
    ///
    /// # Panics
    ///
    /// Panics if not called within a Tokio runtime with IO enabled.
    async fn new(path: &Path, timeout: Duration) -> Result<Self, ConnectionError> {
        let name = path.to_fs_name::<GenericFilePath>().map_err(|source| {
            ConnectionError::InvalidPath {
                source,
                path: path.to_owned(),
            }
        })?;

        let stream = Stream::connect(name)
            .await
            .map_err(|source| ConnectionError::Bind {
                source,
                path: path.to_owned(),
            })?;

        Ok(Self::from_stream(stream, timeout))
    }

    /// Create a connection from an existing byte stream.
    fn from_stream(stream: Stream, timeout: Duration) -> Self {
        Self {
            stream: LengthDelimitedCodec::builder()
                .little_endian()
                .new_framed(stream),
            timeout,
        }
    }

    /// Read the next message from the local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the local socket cannot be read from, the timeout expires, the length
    /// prefix could not be determined, the message was larger than 8 MiB, or the connection closed
    /// unexpectedly.
    async fn read(&mut self) -> Result<BytesMut, std::io::Error> {
        self.stream
            .try_next()
            .timeout(self.timeout)
            .await
            .map_err(Into::into)
            .flatten()?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "local socket closed unexpectedly",
                )
            })
    }

    /// Write a message to the local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the message is larger than 8 MiB, the local socket cannot be written
    /// to, or the timeout expires.
    async fn write(&mut self, bytes: Bytes) -> Result<(), std::io::Error> {
        self.stream.send(bytes).timeout(self.timeout).await??;
        self.stream.flush().timeout(self.timeout).await?
    }
}

/// Error returned when opening a connection to a local socket fails.
#[derive(Error, Debug)]
pub enum ConnectionError {
    /// The given `path` is not supported.
    #[error("path `{}` is not supported for socket communication", .path.display())]
    InvalidPath {
        /// IO error source.
        source: std::io::Error,

        /// Attempted path.
        path: PathBuf,
    },

    /// Error binding to `path`.
    #[error("error binding to path `{}`", .path.display())]
    Bind {
        /// IO error source.
        source: std::io::Error,

        /// Attempted bind path.
        path: PathBuf,
    },
}

/// Messages that can be sent from [`AdminClient`] to [`AdminServer`].
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum Message {
    /// Check that the server is running and the database is open.
    HealthCheck,
}

/// Error when deserializing from bytes fails.
#[derive(Error, Debug)]
#[error("error while deserializing")]
pub struct DeserializeError(#[source] rancor::Error);

/// Error returned during a [health check](AdminClient::health_check()) when the server reports that
/// the database is closed.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
struct DatabaseClosed;

#[cfg(test)]
mod tests {
    use std::error::Error;

    use novelnote_database::Database;
    #[cfg(unix)]
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Run an [`AdminServer`], connect an [`AdminClient`] to it, and execute the closure.
    async fn with_admin_socket<P, F, E>(path: P, f: F) -> Result<(), Box<dyn Error>>
    where
        P: AsRef<Path>,
        F: AsyncFnOnce(&mut AdminClient) -> Result<(), E>,
        E: Into<Box<dyn Error>>,
    {
        #[cfg(unix)]
        let (temp_dir, path) = {
            let temp_dir = TempDir::new()?;
            let path = temp_dir.path().join(path).with_extension("sock");
            (temp_dir, path)
        };

        #[cfg(windows)]
        let path = Path::new(r"\\.\pipe\NovelNote_Admin").join(path);

        let timeout = Duration::from_secs(3);

        let server = AdminServer::bind(&path, timeout, Database::open_in_memory(1).await?)?;
        let cancellation_token = CancellationToken::new();
        let child_token = cancellation_token.child_token();
        let server = tokio::spawn(async move { server.run(&child_token).await });

        let mut client = AdminClient::connect(&path, timeout).await?;
        f(&mut client).await.map_err(Into::into)?;

        cancellation_token.cancel();
        server.await?;
        #[cfg(unix)]
        temp_dir.close()?;

        Ok(())
    }

    #[test_log::test(tokio::test)]
    async fn health_check() -> Result<(), Box<dyn Error>> {
        with_admin_socket("test_health_check", async |client| {
            client.health_check().await
        })
        .await
    }
}
