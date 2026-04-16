//! `novelnote_admin` provides [`AdminServer`] for communication over the admin
//! socket for NovelNote, a self-hosted book tracker.
//!
//! The admin socket's interface is not considered to be a part of this crate's public API.

mod server;

use std::{
    io,
    path::{Path, PathBuf},
};

use bytes::{Bytes, BytesMut};
use futures_util::SinkExt;
use interprocess::local_socket::{
    GenericFilePath, ToFsName, tokio::Stream, traits::tokio::Stream as _,
};
use thiserror::Error;
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub use self::server::AdminServer;

/// Connection between the [`AdminServer`] and client, using a local socket.
#[derive(Debug)]
struct Connection {
    /// Local socket byte stream, framed by a length prefix.
    stream: Framed<Stream, LengthDelimitedCodec>,
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
    #[cfg_attr(not(test), expect(dead_code, reason = "will be used by client"))]
    async fn new(path: &Path) -> Result<Self, ConnectionError> {
        let name = path.to_fs_name::<GenericFilePath>().map_err(|source| {
            ConnectionError::InvalidPath {
                source,
                path: path.to_owned(),
            }
        })?;

        Stream::connect(name)
            .await
            .map(Self::from_stream)
            .map_err(|source| ConnectionError::Bind {
                source,
                path: path.to_owned(),
            })
    }

    /// Create a connection from an existing byte stream.
    fn from_stream(stream: Stream) -> Self {
        Self {
            stream: LengthDelimitedCodec::builder()
                .little_endian()
                .new_framed(stream),
        }
    }

    /// Read the next message from the local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the local socket cannot be read from, the length prefix could not be
    /// determined, the message was larger than 8 MiB, or the connection closed unexpectedly.
    async fn read(&mut self) -> Result<BytesMut, io::Error> {
        self.stream
            .next()
            .await
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "local socket closed unexpectedly",
                )
            })
            .flatten()
    }

    /// Write a message to the local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the message is larger than 8 MiB or the local socket cannot be written
    /// to.
    async fn write(&mut self, bytes: Bytes) -> Result<(), io::Error> {
        self.stream.send(bytes).await?;
        self.stream.flush().await
    }
}

impl From<Stream> for Connection {
    fn from(value: Stream) -> Self {
        Self::from_stream(value)
    }
}

/// Error returned when opening a connection to a local socket fails.
#[derive(Error, Debug)]
pub enum ConnectionError {
    /// The given `path` is not supported.
    #[error("path `{}` is not supported for socket communication", .path.display())]
    InvalidPath {
        /// IO error source.
        source: io::Error,

        /// Attempted path.
        path: PathBuf,
    },

    /// Error binding to `path`.
    #[error("error binding to path `{}`", .path.display())]
    Bind {
        /// IO error source.
        source: io::Error,

        /// Attempted bind path.
        path: PathBuf,
    },
}
