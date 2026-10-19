//! `novelnote_server` is the HTTP server library for NovelNote, a self-hosted book tracker.

mod api;

use std::{
    fmt::{self, Display, Formatter},
    io,
    net::SocketAddr,
    path::PathBuf,
    time::Duration,
};

use axum::{Router, http::StatusCode};
use novelnote_database::Database;
use thiserror::Error;
use tokio::net::TcpListener;
use tower_http::{services::ServeDir, timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, instrument};
use tracing_error::TracedError;

/// Configuration for running the NovelNote HTTP server.
///
/// Use [`Server::run()`] to start it.
#[derive(Debug, Clone)]
pub struct Server {
    /// TCP socket address or Unix domain socket path the server binds to.
    pub bind_address: BindAddress,

    /// Directory to serve from the root.
    ///
    /// Should contain the frontend and any other assets.
    pub asset_directory: PathBuf,

    /// Database connection handle.
    pub database: Database,
}

impl Server {
    /// Start the NovelNote HTTP server, binding to the configured [`SocketAddr`].
    ///
    /// The server will gracefully shut down when the `shutdown_signal` future completes.
    ///
    /// # Errors
    ///
    /// Returns an error if binding to the given [`SocketAddr`] fails.
    #[instrument(
        level = "trace",
        skip_all,
        fields(
            bind_address = %self.bind_address,
            asset_directory = %self.asset_directory.display(),
        ),
    )]
    pub async fn run<F>(self, shutdown_signal: F) -> Result<(), TracedError<ServerError>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            bind_address,
            asset_directory,
            database,
        } = self;

        let router = Router::new()
            .nest(api::PATH, api::router())
            .layer((
                TraceLayer::new_for_http(),
                // Add a timeout so requests cannot stop the server from gracefully shutting down.
                TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    Duration::from_secs(15),
                ),
            ))
            .fallback_service(ServeDir::new(asset_directory))
            .with_state(database);

        match bind_address {
            BindAddress::Tcp(socket_address) => {
                let listener = TcpListener::bind(socket_address).await.map_err(|source| {
                    ServerError::Bind {
                        bind_address: socket_address.into(),
                        source,
                    }
                })?;
                let address = listener.local_addr().map_err(|source| ServerError::Bind {
                    bind_address: socket_address.into(),
                    source,
                })?;

                let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown_signal);
                info!("listening on {address}");
                serve
                    .await
                    .map_err(|source| ServerError::Listen { source })?;
            }
            #[cfg(unix)]
            BindAddress::Unix(path) => {
                use tokio::net::UnixListener;
                use tracing::{debug, warn};

                let listener = match UnixListener::bind(&path) {
                    Ok(listener) => listener,
                    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                        if tokio::fs::remove_file(&path).await.is_ok() {
                            debug!("reclaimed Unix domain socket path");
                            UnixListener::bind(&path).map_err(|source| ServerError::Bind {
                                bind_address: BindAddress::Unix(path.clone()),
                                source,
                            })?
                        } else {
                            return Err(ServerError::Bind {
                                bind_address: BindAddress::Unix(path),
                                source: error,
                            }
                            .into());
                        }
                    }
                    Err(source) => {
                        return Err(ServerError::Bind {
                            bind_address: BindAddress::Unix(path),
                            source,
                        }
                        .into());
                    }
                };

                let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown_signal);
                info!("listening at `{}`", path.display());
                serve
                    .await
                    .map_err(|source| ServerError::Listen { source })?;

                // Check if `path` is an abstract socket path before attempting to remove it.
                let abstract_socket =
                    cfg!(any(target_os = "linux", target_os = "android")) && path.starts_with("\0");
                if !abstract_socket && let Err(error) = tokio::fs::remove_file(&path).await {
                    warn!(%error, "error removing Unix domain socket after closing");
                }
            }
        }

        Ok(())
    }
}

/// TCP socket address or Unix domain socket path a [`Server`] should bind to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindAddress {
    /// Bind the server to a TCP socket.
    Tcp(SocketAddr),

    /// Bind the server to a Unix domain socket.
    #[cfg(unix)]
    Unix(PathBuf),
}

impl Display for BindAddress {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Tcp(socket_addr) => Display::fmt(socket_addr, f),
            #[cfg(unix)]
            Self::Unix(path) => Display::fmt(&path.display(), f),
        }
    }
}

impl From<SocketAddr> for BindAddress {
    fn from(value: SocketAddr) -> Self {
        Self::Tcp(value)
    }
}

/// Error returned from [`Server::run()`].
#[derive(Error, Debug)]
pub enum ServerError {
    /// Error binding to the given address.
    #[error("error binding to `{bind_address}`")]
    Bind {
        /// Address binding was attempted for.
        bind_address: BindAddress,

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
