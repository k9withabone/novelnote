//! The [`Config`] file format and CLI args for NovelNote.

mod admin;
mod log;

use std::{
    borrow::Cow,
    fs::Metadata,
    io,
    net::{IpAddr, SocketAddr},
    num::NonZero,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::Args;
use color_eyre::{
    Section,
    eyre::{Report, WrapErr, eyre},
};
use confique::toml::FormatOptions;
use directories::ProjectDirs;
use jiff::{fmt::serde::tz, tz::TimeZone};
use novelnote_database::Database;
use novelnote_server::{SecretKey, Server};
use serde::{
    Deserialize, Deserializer,
    de::{self, IntoDeserializer},
};
use tokio::task::spawn_blocking;
use tracing::{debug, info, instrument};

use crate::BackupArgs;

pub(crate) use self::{admin::AdminConfig, log::LogConfig};

/// NovelNote configuration.
///
/// Use with `novelnote --config-file <path>`.
///
/// See `novelnote --help` for default config file locations and `novelnote serve --help` for
/// additional explanations of enumerated options.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
pub(crate) struct Config {
    /// Timezone to use when writing timestamps.
    ///
    /// Used by the `stdout` and `file` log output modes and database backup filenames.
    ///
    /// By default, the system timezone is used if it can be determined.
    ///
    /// Does not affect how timestamps are stored in the database (always in UTC) or how they are
    /// displayed to the user (local timezone).
    #[config(
        env = "TZ",
        deserialize_with = tz::required::deserialize,
        layer_attr(arg(
            long,
            env = "TZ",
            visible_alias = "tz",
            value_parser = timezone_value_parser,
            allow_hyphen_values = true,
        ))
    )]
    pub timezone: Option<TimeZone>,

    /// Admin socket configuration.
    ///
    /// By default, `novelnote serve` will, in addition to the HTTP server, start an admin server.
    /// It binds to a Unix domain socket (Linux and macOS) or named pipe (Windows) that other
    /// commands, e.g. `novelnote backup`, use to communicate with the main process.
    #[config(nested, layer_attr(command(flatten)))]
    pub admin: AdminConfig,

    /// Logging configuration.
    #[config(nested, layer_attr(command(flatten)))]
    pub log: LogConfig,

    /// SQLite database configuration.
    #[config(nested, layer_attr(command(flatten)))]
    pub database: DatabaseConfig,

    /// HTTP server configuration.
    #[config(nested, layer_attr(command(flatten)))]
    pub http: HttpServerConfig,
}

/// Parse [`TimeZone`] from a string.
///
/// Used for parsing the `--timezone` CLI arg in [`Config`].
pub(crate) fn timezone_value_parser(value: &str) -> Result<TimeZone, de::value::Error> {
    tz::required::deserialize(value.into_deserializer())
}

/// Partial NovelNote configuration.
type ConfigLayer = <Config as confique::Config>::Layer;

impl From<BackupArgs> for ConfigLayer {
    fn from(
        BackupArgs {
            timezone,
            admin,
            database,
        }: BackupArgs,
    ) -> Self {
        Self {
            timezone,
            admin,
            log: confique::Layer::empty(),
            database,
            http: confique::Layer::empty(),
        }
    }
}

impl Config {
    /// Generate a template TOML config file.
    pub(crate) fn template() -> String {
        confique::toml::template::<Self>(FormatOptions::default())
    }

    /// Load config from CLI args and config files.
    ///
    /// If a config file path is provided, only that file is read. Otherwise, several OS standard
    /// locations and the current working directory are checked for config files.
    pub(crate) fn load(
        cli_args: <Self as confique::Config>::Layer,
        config_file: Option<PathBuf>,
        dirs: &ProjectDirs,
    ) -> Result<Self, confique::Error> {
        load_config(cli_args, config_file, dirs)
    }
}

/// Load config from CLI args and config files.
///
/// If a config file path is provided, only that file is read. Otherwise, several OS standard
/// locations and the current working directory are checked for config files.
fn load_config<T: confique::Config>(
    cli_args: T::Layer,
    config_file: Option<PathBuf>,
    dirs: &ProjectDirs,
) -> Result<T, confique::Error> {
    // Environment variable config is handled by clap.
    let mut builder = T::builder().preloaded(cli_args);

    if let Some(config_file) = config_file {
        builder = builder.file(config_file);
    } else {
        builder = builder
            .file("novelnote.toml")
            .file(dirs.config_dir().join("novelnote.toml"));

        #[cfg(unix)]
        {
            builder = builder.file("/etc/novelnote/novelnote.toml");
        }
    }

    builder.load()
}

/// SQLite database configuration. The `[database]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
#[config(layer_attr(command(next_help_heading = "Database Options")))]
pub(crate) struct DatabaseConfig {
    /// Path to the directory where the SQLite database file is placed.
    ///
    /// If not set, the following directories are used on each platform:
    ///
    /// - `${XDG_DATA_HOME:$HOME/.local/share}/novelnote` (Linux)
    /// - `$HOME/Library/Application Support/NovelNote` (macOS)
    /// - `%RoamingAppData%/NovelNote/data` (Windows)
    ///
    /// If the directory does not exist, it is created.
    ///
    /// The database file itself is named `novelnote.sqlite3`.
    #[config(
        env = "DATABASE_DIRECTORY",
        layer_attr(arg(
            id = "database_directory",
            long = "database-directory",
            env = "DATABASE_DIRECTORY",
            visible_aliases = ["database-dir", "db-directory", "db-dir"],
            value_name = "DB_DIR",
            verbatim_doc_comment,
        ))
    )]
    pub directory: Option<PathBuf>,

    /// Database backup configuration.
    #[config(nested, layer_attr(command(flatten)))]
    pub backup: DatabaseBackupConfig,
}

impl DatabaseConfig {
    /// Get the set database directory or the default.
    pub(crate) fn directory<'a>(&'a self, dirs: &'a ProjectDirs) -> &'a Path {
        self.directory.as_deref().unwrap_or_else(|| dirs.data_dir())
    }

    /// Get the set database backup directory or the default.
    pub(crate) fn backup_directory<'a>(&'a self, dirs: &ProjectDirs) -> Cow<'a, Path> {
        self.backup
            .directory
            .as_deref()
            .map_or_else(|| self.directory(dirs).join("backup").into(), Into::into)
    }

    /// Open a SQLite database in the configured directory.
    #[instrument(name = "open_database", skip_all)]
    pub(crate) async fn open(&self, dirs: &ProjectDirs) -> Result<Database, Report> {
        let directory = self.directory(dirs);
        {
            let span = tracing::Span::current();
            let directory = directory.to_owned();
            spawn_blocking(move || {
                let _entered = span.entered();
                if !directory.is_dir() {
                    std::fs::create_dir_all(&directory).wrap_err_with(|| {
                        format!(
                            "error creating database directory `{}`",
                            directory.display()
                        )
                    })?;
                    info!(database_dir = %directory.display(), "created database directory");
                }
                Ok::<(), Report>(())
            })
            .await??;
        }
        let path = directory.join("novelnote.sqlite3");
        let database = Database::open(path.clone(), 100)
            .await
            .wrap_err_with(|| format!("error opening database at `{}`", path.display()))?;
        debug!(database_path = %path.display(), "opened database");
        Ok(database)
    }
}

/// Database backup configuration. The `[database.backup]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
pub(crate) struct DatabaseBackupConfig {
    /// Path to the directory where the database backups are placed.
    ///
    /// The default is a `backup` directory in the database directory.
    ///
    /// If the directory does not exist, it is created.
    ///
    /// The path must be valid UTF-8 because it is passed to the admin socket.
    ///
    /// Backups are named `novelnote_backup_<ISO-8601 date time>.sqlite3`, with the current date and
    /// time based on the `timezone` set. For example, a backup on January 2nd, 2026 at 3:45pm would
    /// have the filename `novelnote_backup_2026-01-02T154500.sqlite3`.
    #[config(
        env = "DATABASE_BACKUP_DIRECTORY",
        layer_attr(arg(
            id = "database_backup_directory",
            long = "database-backup-directory",
            env = "DATABASE_BACKUP_DIRECTORY",
            visible_aliases = [
                "database-backup-dir",
                "db-backup-directory",
                "db-backup-dir",
                "backup-directory",
                "backup-dir",
            ],
            value_name = "DB_BACKUP_DIR",
        ))
    )]
    pub directory: Option<PathBuf>,

    /// How many database backups to keep.
    ///
    /// Must be at least 1.
    ///
    /// Backups are only considered for deletion if the filename matches the format described above.
    #[config(
        env = "DATABASE_BACKUP_KEEP_LAST",
        default = 5,
        layer_attr(arg(
            long = "database-backup-keep-last",
            env = "DATABASE_BACKUP_KEEP_LAST",
            visible_aliases = [
                "db-backup-keep-last",
                "database-backup-keep",
                "db-backup-keep",
                "keep-last",
            ],
            value_name = "DB_KEEP_LAST",
        ))
    )]
    pub keep_last: NonZero<u8>,
}

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
}

impl HttpServerConfig {
    /// Create [`Server`] from config.
    pub(crate) async fn try_into_server(self, database: Database) -> Result<Server, Report> {
        let Self {
            bind_address,
            port,
            trusted_proxies,
            asset_directory,
        } = self;

        Ok(Server {
            bind_address: bind_address.try_into_server_address(port).await?,
            trusted_proxies: trusted_proxies.into_iter().collect(),
            asset_directory: if let Some(asset_directory) = asset_directory {
                asset_directory
            } else {
                default_asset_directory().await
            },
            // TODO: `password_signup_enabled` from config
            password_signup_enabled: true,
            // TODO: `session_length` from config
            session_length: Duration::from_hours(2),
            // TODO: `secret_key` from file via config
            secret_key: SecretKey::generate(),
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

/// Create a path's parent directory and all its parent components.
///
/// # Errors
///
/// Returns an error if there is an error creating any parent components that do not already exist.
async fn create_parent_dir(path: &Path) -> Result<(), io::Error> {
    if let Some(parent) = path.parent()
        && tokio::fs::metadata(parent)
            .await
            .map_or(true, |parent_meta| !parent_meta.is_dir())
    {
        tokio::fs::create_dir_all(parent).await?;
        info!(dir = %parent.display(), "created directory");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use confique::{Config as _, Layer as _};

    use super::*;

    /// Ensure that all config options are optional or have default values.
    #[test]
    fn all_config_optional() -> Result<(), confique::Error> {
        Config::from_layer(ConfigLayer::default_values()).map(|_| ())
    }
}
