//! `novelnote_server` is the HTTP server library for NovelNote, a self-hosted book tracker.

mod api;

use std::{io, net::SocketAddr, time::Duration};

use axum::{Router, http::StatusCode};
use thiserror::Error;
use tokio::net::TcpListener;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, instrument};
use tracing_error::TracedError;

/// Configuration for running the NovelNote HTTP server.
///
/// Use [`Server::run()`] to start it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    /// Socket address the server binds to.
    pub socket_address: SocketAddr,
}

impl Server {
    /// Start the NovelNote HTTP server, binding to the configured [`SocketAddr`].
    ///
    /// The server will gracefully shut down when the `shutdown_signal` future completes.
    ///
    /// # Errors
    ///
    /// Returns an error if binding to the given [`SocketAddr`] fails.
    #[instrument(level = "trace", skip(shutdown_signal))]
    pub async fn run<F>(self, shutdown_signal: F) -> Result<(), TracedError<ServerError>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self { socket_address } = self;

        let router = Router::new().nest(api::PATH, api::router()).layer((
            TraceLayer::new_for_http(),
            // Add a timeout so requests cannot stop the server from gracefully shutting down.
            TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(15)),
        ));

        let listener =
            TcpListener::bind(socket_address)
                .await
                .map_err(|source| ServerError::Bind {
                    socket_address,
                    source,
                })?;
        let address = listener.local_addr().map_err(|source| ServerError::Bind {
            socket_address,
            source,
        })?;

        let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown_signal);
        info!("listening on {address}");
        serve
            .await
            .map_err(|source| ServerError::Listen { source })?;

        Ok(())
    }
}

/// Error returned from [`Server::run()`].
#[derive(Error, Debug)]
pub enum ServerError {
    /// Error binding to the given socket address.
    #[error("error binding to socket address `{socket_address}`")]
    Bind {
        /// Socket address binding was attempted for.
        socket_address: SocketAddr,

        /// Error source.
        source: io::Error,
    },

    /// Error listening to the TCP stream.
    #[error("error listening to TCP stream")]
    Listen {
        /// Error source.
        source: io::Error,
    },
}
