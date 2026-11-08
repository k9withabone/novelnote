//! `novelnote_server` is the HTTP server library for NovelNote, a self-hosted book tracker.

mod api;

use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    extract::{ConnectInfo, FromRef},
    http::{HeaderName, Request, StatusCode},
};
use axum_extra::extract::cookie;
use itertools::{Either, Itertools};
use novelnote_database::Database;
use thiserror::Error;
use tokio::{net::TcpListener, sync::oneshot};
use tower::util::MapRequestLayer;
use tower_http::{
    services::ServeDir,
    timeout::TimeoutLayer,
    trace::{HttpMakeClassifier, MakeSpan, TraceLayer},
};
use tracing::{debug_span, info, instrument};
use tracing_error::TracedError;

use crate::api::AuthConfig;

/// Identifies the originating IP address of a client connecting through a proxy server.
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// Configuration for running the NovelNote HTTP server.
///
/// Use [`Server::run()`] to start it.
#[derive(Debug, Clone)]
pub struct Server {
    /// TCP socket address or Unix domain socket path the server binds to.
    pub bind_address: BindAddress,

    /// IP addresses of proxies trusted to set the `X-Forwarded-For` header correctly.
    ///
    /// This is used to determine the originating IP address of a client for logging purposes.
    pub trusted_proxies: HashSet<IpAddr>,

    /// Directory to serve from the root.
    ///
    /// Should contain the frontend and any other assets.
    pub asset_directory: PathBuf,

    /// Whether signups using a password should be enabled.
    pub password_signup_enabled: bool,

    /// How long between requests a session should be valid for before expiring.
    pub session_length: Duration,

    /// Secret key used for securing session cookies, tokens, and as a password pepper.
    ///
    /// If this is changed, all existing sessions, tokens, and passwords become invalid.
    pub secret_key: SecretKey,

    /// Database connection handle.
    pub database: Database,
}

impl Server {
    /// Start the NovelNote HTTP server, binding to the configured [`BindAddress`].
    ///
    /// A message will be sent with `ready_sender` when the server is ready to receive requests.
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
    pub async fn run<F>(
        self,
        ready_sender: oneshot::Sender<()>,
        shutdown_signal: F,
    ) -> Result<(), TracedError<ServerError>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            bind_address,
            trusted_proxies,
            asset_directory,
            password_signup_enabled,
            session_length,
            secret_key,
            database,
        } = self;

        let state = AppState {
            auth_config: AuthConfig {
                password_signup_enabled,
                secret_key,
                session_length,
            },
            database,
        };

        let router = Router::new()
            .nest(api::PATH, api::router())
            .layer((
                ClientIp::layer(trusted_proxies),
                trace_layer(),
                // Add a timeout so requests cannot stop the server from gracefully shutting down.
                TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    Duration::from_secs(15),
                ),
            ))
            .fallback_service(ServeDir::new(asset_directory))
            .with_state(state);

        match bind_address {
            BindAddress::Tcp(socket_address) => {
                bind_tcp(socket_address, router, ready_sender, shutdown_signal).await?;
            }
            #[cfg(unix)]
            BindAddress::Unix(path) => {
                bind_unix(path, router, ready_sender, shutdown_signal).await?;
            }
        }

        Ok(())
    }
}

/// Bind a [`TcpListener`] to the socket and serve the `router` on it.
///
/// # Errors
///
/// Returns an error if the socket address cannot be bound to.
#[instrument(level = "trace", skip_all)]
async fn bind_tcp(
    socket_address: SocketAddr,
    router: Router,
    ready_sender: oneshot::Sender<()>,
    shutdown_signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let listener = TcpListener::bind(socket_address)
        .await
        .map_err(|source| ServerError::Bind {
            bind_address: socket_address.into(),
            source,
        })?;
    let address = listener.local_addr().map_err(|source| ServerError::Bind {
        bind_address: socket_address.into(),
        source,
    })?;

    let serve = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal);

    info!("listening on {address}");

    #[expect(unused_must_use, reason = "ok if readiness message is not received")]
    ready_sender.send(());

    serve
        .await
        .map_err(|source| ServerError::Listen { source })?;

    Ok(())
}

/// Bind a [`UnixListener`](tokio::net::UnixListener) to a Unix domain socket at the given `path`
/// and serve the `router` on it.
///
/// # Errors
///
/// Returns an error if the Unix domain socket cannot be created.
#[cfg(unix)]
#[instrument(level = "trace", skip_all)]
async fn bind_unix(
    path: PathBuf,
    router: Router,
    ready_sender: oneshot::Sender<()>,
    shutdown_signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
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
                });
            }
        }
        Err(source) => {
            return Err(ServerError::Bind {
                bind_address: BindAddress::Unix(path),
                source,
            });
        }
    };

    let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown_signal);

    info!("listening at `{}`", path.display());

    #[expect(unused_must_use, reason = "ok if readiness message is not received")]
    ready_sender.send(());

    serve
        .await
        .map_err(|source| ServerError::Listen { source })?;

    // Check if `path` is an abstract socket path before attempting to remove it.
    let abstract_socket =
        cfg!(any(target_os = "linux", target_os = "android")) && path.starts_with("\0");
    if !abstract_socket && let Err(error) = tokio::fs::remove_file(&path).await {
        warn!(%error, "error removing Unix domain socket after closing");
    }

    Ok(())
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

/// Secret key to use for cryptographic purposes.
///
/// It should be loaded from a file of bytes created with a cryptographically secure pseudorandom
/// number generator (CSPRNG), such as the one used in [`SecretKey::generate()`].
///
/// They must be at least 64 bytes and less than 4 GiB long.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(Arc<[u8]>);

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_tuple("SecretKey").field(&"REDACTED").finish()
    }
}

impl SecretKey {
    /// Minimum length a [`SecretKey`] can be.
    pub const MIN_LEN: usize = 64;

    /// Maximum length a [`SecretKey`] can be.
    pub const MAX_LEN: usize = 0xFFFF_FFFF;

    /// Create a secret key.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is less than 64 bytes or more than 4 GiB long.
    pub fn new<T: Into<Arc<[u8]>>>(key: T) -> Result<Self, SecretKeyLengthError> {
        let key = key.into();
        if key.len() < Self::MIN_LEN || key.len() > Self::MAX_LEN {
            Err(SecretKeyLengthError)
        } else {
            Ok(Self(key))
        }
    }

    /// Generate a secret key of minimum length using random values.
    #[must_use]
    pub fn generate() -> Self {
        let mut buf = [0; Self::MIN_LEN];
        rand::fill(&mut buf);
        Self(buf.into())
    }

    /// Secret key as a byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Error returned when creating a [`SecretKey`] fails.
///
/// [`SecretKey`]s must be at least 64 bytes and less than 4 GiB long.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("secret key must be at least 64 bytes and less than 4 GiB long")]
pub struct SecretKeyLengthError;

impl AsRef<[u8]> for SecretKey {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl TryFrom<Vec<u8>> for SecretKey {
    type Error = SecretKeyLengthError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// [`State`](axum::extract::State) for NovelNote routes.
#[derive(Debug, Clone)]
struct AppState {
    /// Configuration for `/api/auth` routes.
    auth_config: AuthConfig,

    /// Database connection handle.
    database: Database,
}

impl FromRef<AppState> for AuthConfig {
    fn from_ref(input: &AppState) -> Self {
        input.auth_config.clone()
    }
}

impl FromRef<AppState> for Database {
    fn from_ref(input: &AppState) -> Self {
        input.database.clone()
    }
}

impl FromRef<AppState> for cookie::Key {
    fn from_ref(input: &AppState) -> Self {
        // NO_PANIC: `SecretKey` is at least 64 bytes long.
        Self::from(input.auth_config.secret_key.as_bytes())
    }
}

/// Client's IP address from [`ConnectInfo`] and `X-Forwarded-For` headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientIp(Option<IpAddr>);

impl ClientIp {
    /// Create a [`Layer`](tower::Layer) which adds a [`ClientIp`] extension to a [`Request`].
    fn layer<B>(
        trusted_proxies: HashSet<IpAddr>,
    ) -> MapRequestLayer<impl Fn(Request<B>) -> Request<B> + Clone> {
        let trusted_proxies = Arc::new(trusted_proxies);

        MapRequestLayer::new(move |mut request| {
            let client_ip = Self::from_request(&request, &trusted_proxies);
            request.extensions_mut().insert(client_ip);
            request
        })
    }

    /// Get the client's IP address from [`ConnectInfo`] and `X-Forwarded-For` headers.
    fn from_request<B>(request: &Request<B>, trusted_proxies: &HashSet<IpAddr>) -> Self {
        // `ConnectInfo` only available when listening on TCP socket.
        let connect_info = request
            .extensions()
            .get()
            .map(|ConnectInfo::<SocketAddr>(socket)| socket.ip());

        // Implementation of "Selecting an IP address" from the MDN documentation for
        // `X-Forwarded-For`: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/X-Forwarded-For#selecting_an_ip_address
        let proxies = request
            .headers()
            .get_all(X_FORWARDED_FOR)
            .iter()
            .map(|value| value.to_str().map(|value| value.split(',')))
            .flatten_ok()
            .rev()
            .map_while(|value| value.ok()?.trim().parse().ok());
        let proxies = if connect_info.is_some() {
            Either::Left(std::iter::chain(connect_info, proxies))
        } else {
            Either::Right(proxies)
        };
        let client_ip = proxies.find_or_last(|proxy| !trusted_proxies.contains(proxy));

        Self(client_ip)
    }
}

/// Create a [`TraceLayer`] which includes the [`ClientIp`] in the span fields.
fn trace_layer<B>() -> TraceLayer<HttpMakeClassifier, impl MakeSpan<B> + Clone> {
    TraceLayer::new_for_http().make_span_with(|request: &Request<B>| {
        let ClientIp(client_ip) = request
            .extensions()
            .get()
            .expect("`ClientIP` extension should be set");

        debug_span!(
            "request",
            client_ip = client_ip.map(tracing::field::display),
            method = %request.method(),
            uri = %request.uri(),
            version = ?request.version(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests for the [`ClientIp`] extractor.
    mod client_ip {
        use axum::http;

        use super::*;

        /// Build a [`Request`] and construct a [`ClientIp`].
        fn client_ip(
            trusted: &[[u8; 4]],
            connect_info: Option<[u8; 4]>,
            proxies: &str,
        ) -> Result<ClientIp, http::Error> {
            let trusted = trusted.iter().copied().map_into().collect();

            let mut builder = Request::builder();
            if let Some(ip) = connect_info {
                builder = builder.extension(ConnectInfo(SocketAddr::new(ip.into(), 8080)));
            }
            if !proxies.is_empty() {
                builder = builder.header(X_FORWARDED_FOR, proxies);
            }
            let request = builder.body(())?;

            Ok(ClientIp::from_request(&request, &trusted))
        }

        #[test]
        fn none() -> Result<(), http::Error> {
            assert_eq!(client_ip(&[], None, "")?, ClientIp(None),);
            Ok(())
        }

        #[test]
        fn connect_info() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(&[], Some(client), "")?,
                ClientIp(Some(client.into())),
            );
            Ok(())
        }

        #[test]
        fn simple() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(&[client], Some(client), "192.168.0.1")?,
                ClientIp(Some([192, 168, 0, 1].into())),
            );
            Ok(())
        }

        #[test]
        fn first_untrusted() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(&[client], Some(client), "192.168.0.1, 192.168.0.2")?,
                ClientIp(Some([192, 168, 0, 2].into())),
            );
            Ok(())
        }

        #[test]
        fn last_trusted() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(
                    &[client, [192, 168, 0, 1], [192, 168, 0, 2]],
                    Some(client),
                    "192.168.0.1, 192.168.0.2",
                )?,
                ClientIp(Some([192, 168, 0, 1].into())),
            );
            Ok(())
        }

        #[test]
        fn stop_at_malformed() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(
                    &[client, [192, 168, 0, 3]],
                    Some(client),
                    "192.168.0.1, 192.168.0.x, 192.168.0.3",
                )?,
                ClientIp(Some([192, 168, 0, 3].into())),
            );
            Ok(())
        }

        #[test]
        fn after_trusted() -> Result<(), http::Error> {
            let client = [127, 0, 0, 1];
            assert_eq!(
                client_ip(
                    &[client, [192, 168, 0, 3]],
                    Some(client),
                    "192.168.0.1, 192.168.0.2, 192.168.0.3",
                )?,
                ClientIp(Some([192, 168, 0, 2].into())),
            );
            Ok(())
        }
    }
}
