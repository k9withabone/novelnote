//! [`AdminClient`] implementation.

use std::{path::Path, time::Duration};

use bytes::{Bytes, BytesMut};
use rkyv::{
    Archive, Deserialize,
    api::high::HighValidator,
    bytecheck::CheckBytes,
    de::Pool,
    rancor::{self, Strategy},
};
use thiserror::Error;
use tracing::instrument;

use crate::{
    Connection, ConnectionError, DatabaseClosed, DeserializeError, Message, ResponseError,
};

/// Admin socket client for NovelNote. Connects to a local socket.
#[derive(Debug)]
pub struct AdminClient {
    /// Connection to the admin socket.
    connection: Connection,
}

impl AdminClient {
    /// Connect to an [`AdminServer`](crate::AdminServer) bound to a local socket at `path`.
    ///
    /// Connection attempts will time out after `timeout` expires.
    ///
    /// # Errors
    ///
    /// Returns an error if the `path` is invalid or cannot be bound to.
    ///
    /// # Panics
    ///
    /// Panics if not called within a Tokio runtime with IO enabled.
    #[instrument(level = "debug", skip_all, fields(path = %path.display()), err)]
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, ConnectionError> {
        Ok(Self {
            connection: Connection::new(path, timeout).await?,
        })
    }

    /// Send a health check request to the admin socket to see if it can respond and that the
    /// server's database connection is open.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection times out, the request cannot be sent, the response
    /// cannot be read or deserialized, or the server's database connection is closed.
    pub async fn health_check(&mut self) -> Result<(), HealthCheckError> {
        self.request_response_deserialize::<Result<(), DatabaseClosed>>(&Message::HealthCheck)
            .await?
            .map_err(Into::into)
    }

    /// Send a request to the admin socket to backup the database to `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection times out, the request cannot be sent, the response
    /// cannot be read or deserialized, the server's database connection is closed, or the database
    /// backup fails.
    pub async fn backup(&mut self, path: String) -> Result<(), RequestError> {
        self.request_response_deserialize::<Result<(), ResponseError>>(&Message::Backup { path })
            .await?
            .map_err(Into::into)
    }

    /// Send a message to the [`AdminServer`](crate::AdminServer) using the [`Connection`] to the
    /// local socket, wait for a response, and deserialize it.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be written to or read from, or the connection times
    /// out.
    async fn request_response_deserialize<T>(
        &mut self,
        message: &Message,
    ) -> Result<T, CommunicationError>
    where
        T: Archive,
        T::Archived: for<'a> CheckBytes<HighValidator<'a, rancor::Error>>
            + Deserialize<T, Strategy<Pool, rancor::Error>>,
    {
        let bytes = self.request_response(message).await?;
        rkyv::from_bytes(&bytes).map_err(CommunicationError::from_deserialize)
    }

    /// Send a message to the [`AdminServer`](crate::AdminServer) using the [`Connection`] to the
    /// local socket and wait for a response.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be written to or read from, or the connection times
    /// out.
    async fn request_response(
        &mut self,
        message: &Message,
    ) -> Result<BytesMut, CommunicationError> {
        self.send(message).await?;
        self.receive().await.map_err(Into::into)
    }

    /// Send a message to the [`AdminServer`](crate::AdminServer) using the [`Connection`] to the
    /// local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be written to or the connection times out.
    async fn send(&mut self, message: &Message) -> Result<(), SendError> {
        let bytes = Bytes::from_owner(
            rkyv::to_bytes::<rancor::Error>(message)
                .expect("serializing `Message` to bytes cannot fail"),
        );
        self.connection.write(bytes).await.map_err(Into::into)
    }

    /// Receive a response from the [`AdminServer`](crate::AdminServer) using the [`Connection`] to
    /// the local socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be read from or the connection times out.
    async fn receive(&mut self) -> Result<BytesMut, ReceiveError> {
        self.connection.read().await.map_err(Into::into)
    }
}

/// Error returned when [`AdminClient::health_check()`] fails.
#[derive(Error, Debug)]
pub enum HealthCheckError {
    /// Error communicating with the admin socket.
    #[error("error communicating with admin socket")]
    Communication(#[from] CommunicationError),

    /// Server's database connection is closed, server restart recommended.
    #[error("server's database connection is closed, server restart recommended")]
    DatabaseClosed,
}

impl From<DatabaseClosed> for HealthCheckError {
    fn from(_: DatabaseClosed) -> Self {
        Self::DatabaseClosed
    }
}

/// Error returned when a request to the admin socket fails or the response is an error.
#[derive(Error, Debug)]
pub enum RequestError {
    /// Error communicating with the admin socket.
    #[error("error communicating with admin socket")]
    Communication(#[from] CommunicationError),

    /// The database returned an error when processing the request.
    #[error("database error: {error_message}")]
    Database {
        /// Message from the database error.
        error_message: String,
    },

    /// The server's database connection is closed, server restart recommended.
    #[error("database connection is closed, server restart recommended")]
    DatabaseClosed,
}

impl From<ResponseError> for RequestError {
    fn from(value: ResponseError) -> Self {
        match value {
            ResponseError::Database { error_message } => Self::Database { error_message },
            ResponseError::DatabaseClosed => Self::DatabaseClosed,
        }
    }
}

/// Error communicating with the admin socket.
#[derive(Error, Debug)]
pub enum CommunicationError {
    /// Error sending message.
    #[error("error sending message")]
    Send(#[from] SendError),

    /// Error receiving response.
    #[error("error receiving response")]
    Receive(#[from] ReceiveError),

    /// Error deserializing the response.
    #[error("error deserializing response")]
    Deserialize(#[from] DeserializeError),
}

impl CommunicationError {
    /// Error deserializing the response.
    const fn from_deserialize(error: rancor::Error) -> Self {
        Self::Deserialize(DeserializeError(error))
    }
}

/// Error when sending a message to the [`AdminServer`](crate::AdminServer).
#[derive(Error, Debug)]
#[error("error sending message to admin server")]
pub struct SendError(#[from] pub std::io::Error);

/// Error when receiving a response from the [`AdminServer`](crate::AdminServer).
#[derive(Error, Debug)]
#[error("error receiving response from admin server")]
pub struct ReceiveError(#[from] pub std::io::Error);
