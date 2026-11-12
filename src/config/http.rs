//! [`HttpServerConfig`] and related types.

use std::{
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::Args;
use color_eyre::{
    Section,
    eyre::{Report, WrapErr, eyre},
};
use novelnote_database::Database;
use novelnote_server::{SecretKey, Server};
use serde::{Deserialize, Deserializer, de};
use tokio::task::spawn_blocking;
use tracing::warn;

use super::{Timeout, create_parent_dir};

/// HTTP server configuration. The `[http]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
#[config(layer_attr(command(next_help_heading = "HTTP Server Options")))]
pub(crate) struct HttpServerConfig {
    /// IP address or Unix domain socket path the HTTP server should bind to.
    ///
    /// Unix domain socket paths must start with `/`, `./`, or `../`. They are only supported on
    /// Unix (Linux and macOS).
    ///
    /// Defaults to binding to all IP interfaces (`0.0.0.0`).
    #[config(
        env = "HTTP_ADDRESS",
        default = "0.0.0.0",
        layer_attr(arg(
            short = 'a',
            long,
            env = "HTTP_ADDRESS",
            visible_alias = "http-address",
        ))
    )]
    pub bind_address: BindAddress,

    /// TCP port the HTTP server should bind to.
    #[config(
        env = "HTTP_PORT",
        default = 8080,
        layer_attr(arg(short, long, env = "HTTP_PORT", visible_alias = "http-port"))
    )]
    pub port: u16,

    /// IP addresses of proxies trusted to set the `X-Forwarded-For` header correctly.
    ///
    /// This is used to determine the originating IP address of a client for logging purposes.
    ///
    /// If not set, the connecting IP address is used as the client's. If NovelNote is set up behind
    /// a reverse proxy, the connecting IP address will always be the reverse proxy's IP address.
    ///
    /// When setting via environment variable or CLI option, values may be delimited by a comma (,).
    #[config(
        env = "TRUSTED_PROXIES",
        default = [],
        layer_attr(arg(long, env, value_delimiter = ','))
    )]
    pub trusted_proxies: Vec<IpAddr>,

    /// Path to the directory containing NovelNote's frontend and other assets.
    ///
    /// Defaults to `/usr/share/novelnote/dist` or `/usr/local/share/novelnote/dist` on Linux and
    /// macOS if they exist, otherwise the default is `dist` in the current working directory.
    #[config(
        env = "ASSET_DIRECTORY",
        layer_attr(arg(
            long,
            env = "ASSET_DIRECTORY",
            visible_aliases = ["asset-dir", "assets"],
            value_name = "ASSET_DIR",
        ))
    )]
    pub asset_directory: Option<PathBuf>,

    /// Whether user signups using a password are enabled (default).
    ///
    /// Disabling password signups causes the web frontend to not show a signup option and the
    /// `POST /api/auth/signup` route to return a `403: Forbidden` status.
    #[config(
        env = "PASSWORD_SIGNUPS_ENABLED",
        default = true,
        layer_attr(arg(long, env = "PASSWORD_SIGNUPS_ENABLED", value_name = "ENABLED"))
    )]
    pub enable_password_signups: bool,

    /// How long between requests a session should be valid for before expiring.
    ///
    /// For example, if a user logs in and then does nothing until the session expires, they will
    /// have to log in again before accessing NovelNote.
    ///
    /// See the [`jiff` "friendly" format] for examples of how this may be specified.
    ///
    /// The default is 2 hours. The maximum is 1 week.
    ///
    /// [`jiff` "friendly" format]: https://docs.rs/jiff/latest/jiff/fmt/friendly/index.html
    #[config(
        env = "SESSION_LENGTH",
        default = "2 hours",
        layer_attr(arg(long, env))
    )]
    pub session_length: Timeout<{ Duration::from_hours(24 * 7).as_secs() }>,

    /// Path to a secret key used for securing session cookies, tokens, and as a password pepper.
    ///
    /// If the secret key is changed, all existing sessions, tokens, and passwords become invalid.
    ///
    /// Secret keys must be at least 64 bytes and smaller than 4 GiB.
    ///
    /// If not set, NovelNote will first check the directory specified by the
    /// `CREDENTIALS_DIRECTORY` environment variable, if set (e.g. by systemd), for a `secret_key`
    /// or `novelnote_secret_key` file. Then, it will check the `/run/secrets` directory (used for
    /// Podman / Docker secrets), if it exists, for the same files. Finally, it will use a
    /// `secret_key` file in the database directory as a default.
    ///
    /// If the specified or default file does not exist, a minimum sized secret key is generated
    /// and written to a new file. Make sure that this generated secret key is persisted or any
    /// created sessions, tokens, or passwords will be invalid when NovelNote is restarted.
    #[config(env = "SECRET_KEY_FILE", layer_attr(arg(long, env)))]
    pub secret_key_file: Option<PathBuf>,
}

impl HttpServerConfig {
    /// Create [`Server`] from config.
    pub(crate) async fn try_into_server(
        self,
        database_directory: &Path,
        database: Database,
    ) -> Result<Server, Report> {
        let Self {
            bind_address,
            port,
            trusted_proxies,
            asset_directory,
            enable_password_signups: password_signup_enabled,
            session_length,
            secret_key_file,
        } = self;

        let asset_directory = if let Some(asset_directory) = asset_directory {
            asset_directory
        } else {
            default_asset_directory().await
        };

        let secret_key_file = if let Some(path) = secret_key_file {
            path
        } else {
            default_secret_key_file(database_directory).await
        };

        Ok(Server {
            bind_address: bind_address.try_into_server_address(port).await?,
            trusted_proxies: trusted_proxies.into_iter().collect(),
            asset_directory,
            password_signup_enabled,
            session_length: session_length.into(),
            secret_key: read_secret_key_or_generate(&secret_key_file).await?,
            database,
        })
    }
}

/// Get the default asset directory. On Unix, it is `/usr/share/novelnote/dist` or
/// `/usr/local/share/novelnote/dist` if they exist. Otherwise it is `dist`.
#[cfg_attr(not(unix), expect(clippy::unused_async, reason = "cfg"))]
async fn default_asset_directory() -> PathBuf {
    #[cfg(unix)]
    {
        let dirs = [
            "/usr/share/novelnote/dist",
            "/usr/local/share/novelnote/dist",
        ];

        for dir in dirs {
            use std::fs::Metadata;

            if tokio::fs::metadata(dir)
                .await
                .as_ref()
                .is_ok_and(Metadata::is_dir)
            {
                return PathBuf::from(dir);
            }
        }
    }

    PathBuf::from("dist")
}

/// Get the default [`SecretKey`] file path.
///
/// If the `CREDENTIALS_DIRECTORY` environment variable (used by systemd credentials) is set, then
/// the directory is searched for a `secret_key` or `novelnote_secret_key` file.
///
/// If the `/run/secrets` directory (used by Podman / Docker secrets) exists, then it is searched
/// for a `secret_key` or `novelnote_secret_key` file.
///
/// If none of those files exist, then a path for a `secret_key` file in the `database_directory` is
/// returned.
async fn default_secret_key_file(database_directory: &Path) -> PathBuf {
    spawn_blocking(|| {
        // systemd credential
        let credentials_directory = std::env::var("CREDENTIALS_DIRECTORY").ok();
        let credentials_directory = credentials_directory.as_deref().map(Path::new);

        // Podman / Docker secret
        let secrets_directory = Path::new("/run/secrets");

        for directory in std::iter::chain(credentials_directory, Some(secrets_directory)) {
            let secret_key_file = directory.join("secret_key");
            if secret_key_file.is_file() {
                return Some(secret_key_file);
            }

            let secret_key_file = directory.join("novelnote_secret_key");
            if secret_key_file.is_file() {
                return Some(secret_key_file);
            }
        }

        None
    })
    .await
    .expect("panicked getting default secret key file from env")
    .unwrap_or_else(|| database_directory.join("secret_key"))
}

/// Read a [`SecretKey`] from the given `path`.
///
/// If the file does not exist, then a new [`SecretKey`] is generated and written to the `path`.
///
/// # Errors
///
/// Returns an error if the file exists and could not be read, or does not exist and could not be
/// created.
async fn read_secret_key_or_generate(path: &Path) -> Result<SecretKey, Report> {
    match tokio::fs::read(path).await {
        Ok(bytes) => bytes
            .try_into()
            .wrap_err_with(|| format!("error reading file `{}` as secret key", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let secret_key = SecretKey::generate();
            if let Err(error) = tokio::fs::write(path, &secret_key).await {
                Err(error).wrap_err_with(|| {
                    format!(
                        "secret key file `{}` could not found and there was an error writing a \
                         generated secret key",
                        path.display()
                    )
                })
            } else {
                warn!(
                    path = %path.display(),
                    "generated new secret key, this file must be persisted for sessions and \
                     passwords to persist"
                );
                Ok(secret_key)
            }
        }
        Err(error) => Err(error)
            .wrap_err_with(|| format!("error reading secret key file `{}`", path.display())),
    }
}

/// IP address or Unix domain socket path for [`HttpServerConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindAddress {
    /// Bind HTTP server to an IP address.
    Ip(IpAddr),

    /// Bind HTTP server to Unix domain socket path.
    Unix(PathBuf),
}

impl BindAddress {
    /// Attempt to convert into a [`Server`] bind address.
    ///
    /// # Errors
    ///
    /// Returns an error if the Unix domain socket path's parent directory could not be created or a
    /// Unix domain socket path was given on a non-Unix system.
    async fn try_into_server_address(
        self,
        port: u16,
    ) -> Result<novelnote_server::BindAddress, Report> {
        match self {
            Self::Ip(ip) => Ok(SocketAddr::new(ip, port).into()),
            Self::Unix(path) if cfg!(unix) => {
                create_parent_dir(&path).await.wrap_err_with(|| {
                    format!(
                        "error creating parent directory for Unix domain socket `{}`",
                        path.display(),
                    )
                })?;
                Ok(novelnote_server::BindAddress::Unix(path))
            }
            Self::Unix(path) => Err(eyre!("cannot bind HTTP server to `{}`", path.display()))
                .warning(
                    "binding the HTTP server to a Unix domain socket path is only supported on \
                     Unix systems",
                ),
        }
    }
}

impl FromStr for BindAddress {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(ip) => Ok(Self::Ip(ip)),
            Err(_) if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") => {
                Ok(Self::Unix(s.into()))
            }
            Err(error) => Err(eyre!(
                // Normally, including the error in the `Report` message is bad practice, but in
                // this case it is necessary as sources are not displayed by `clap` errors.
                "error parsing `{s}` as an IP address: {error}; if the address is a Unix domain \
                 socket path, it must start with `/`, `./`, or `../`"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for BindAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}
