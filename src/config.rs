//! The [`Config`] file format and CLI args for NovelNote.

use std::{
    borrow::Cow,
    fs::Metadata,
    net::{IpAddr, SocketAddr},
    num::NonZero,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::{Args, ValueEnum};
use color_eyre::{
    Section,
    eyre::{Report, WrapErr, ensure},
};
use confique::toml::FormatOptions;
use directories::ProjectDirs;
use jiff::{Span, Timestamp, Unit, fmt::serde::tz, tz::TimeZone};
use novelnote_admin::{AdminClient, AdminServer};
use novelnote_database::Database;
use novelnote_server::Server;
use serde::{
    Deserialize, Deserializer,
    de::{self, IntoDeserializer},
};
use tokio::task::spawn_blocking;
use tracing::{debug, info, instrument};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    EnvFilter,
    filter::Directive,
    fmt::{format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::BackupArgs;

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

/// Admin socket configuration. The `[admin]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
#[config(layer_attr(command(next_help_heading = "Admin Socket Options")))]
pub(crate) struct AdminConfig {
    /// Control whether the admin socket is enabled (default).
    ///
    /// Disabling the admin socket is not recommended as it is required for some commands, e.g.
    /// `novelnote backup`, to function.
    #[config(
        env = "ADMIN_SOCKET_ENABLED",
        default = true,
        layer_attr(arg(long = "enable-admin-socket", env = "ADMIN_SOCKET_ENABLED"))
    )]
    pub enabled: bool,

    /// Admin socket path.
    ///
    /// On Linux and macOS, this is the path of the Unix domain socket. If the directory for the
    /// socket does not exist, it is created. For Linux, the default is
    /// `${XDG_RUNTIME_DIR}/novelnote/novelnote_admin.sock`. For macOS the default is
    /// `$HOME/Library/Application Support/NovelNote/novelnote_admin.sock`.
    ///
    /// On Windows, this is the named pipe namespace. It must start with `\\.\pipe\` and the default
    /// is `\\.\pipe\NovelNote_Admin`.
    #[config(
        env = "ADMIN_SOCKET_PATH",
        layer_attr(arg(
            long = "admin-socket-path",
            env = "ADMIN_SOCKET_PATH",
            visible_aliases = ["admin-path", "socket-path"],
            value_name = "ADMIN_SOCKET_PATH",
        ))
    )]
    pub socket_path: Option<AdminSocketPath>,

    /// How long each connection will attempt to read or write before timing out.
    ///
    /// See the [`jiff` "friendly" format] for examples of how this may be specified.
    ///
    /// The default is 5 seconds. It must be less than 1 minute.
    ///
    /// [`jiff` "friendly" format]: https://docs.rs/jiff/latest/jiff/fmt/friendly/index.html
    #[config(
        env = "ADMIN_SOCKET_TIMEOUT",
        default = "5 seconds",
        layer_attr(arg(
            long = "admin-socket-timeout",
            env = "ADMIN_SOCKET_TIMEOUT",
            visible_alias = "admin-timeout",
        ))
    )]
    pub timeout: Timeout<60>,
}

/// [`AdminConfig`] nested in the `[admin]` table.
///
/// Used for deserializing a part of a [`Config`] file.
#[derive(confique::Config)]
struct NestedAdminConfig {
    /// Admin socket configuration.
    #[config(nested)]
    admin: AdminConfig,
}

/// Partial configuration for [`NestedAdminConfig`].
type NestedAdminConfigLayer = <NestedAdminConfig as confique::Config>::Layer;

impl AdminConfig {
    /// Load admin config from CLI args and config files.
    ///
    /// If a config file path is provided, only that file is read. Otherwise, several OS standard
    /// locations and the current working directory are checked for config files.
    pub(crate) fn load(
        cli_args: <Self as confique::Config>::Layer,
        config_file: Option<PathBuf>,
        dirs: &ProjectDirs,
    ) -> Result<Self, confique::Error> {
        let cli_args = NestedAdminConfigLayer { admin: cli_args };
        let NestedAdminConfig { admin } = load_config(cli_args, config_file, dirs)?;
        Ok(admin)
    }

    /// Bind an [`AdminServer`] using the configured values.
    ///
    /// Returns [`None`] if the admin socket is disabled.
    ///
    /// # Errors
    ///
    /// Returns an error if there is an error binding the admin server.
    pub(crate) fn into_server(
        self,
        dirs: &ProjectDirs,
        database: Database,
    ) -> Result<Option<AdminServer>, Report> {
        let Self {
            enabled,
            socket_path,
            timeout,
        } = self;

        if enabled {
            let socket_path = socket_path.unwrap_or_else(|| AdminSocketPath::default(dirs));
            socket_path.create_dir()?;

            AdminServer::bind(socket_path.as_ref(), timeout.into(), database)
                .map(Some)
                .wrap_err("error binding admin server")
        } else {
            Ok(None)
        }
    }

    /// Connect an [`AdminClient`] to the admin socket using the configured values.
    ///
    /// Returns [`None`] if the admin socket is disabled.
    ///
    /// # Errors
    ///
    /// Returns an error if a connection to the admin socket cannot be established.
    pub(crate) async fn into_client(
        self,
        dirs: &ProjectDirs,
    ) -> Result<Option<AdminClient>, Report> {
        let Self {
            enabled,
            socket_path,
            timeout,
        } = self;

        if enabled {
            let socket_path = socket_path.unwrap_or_else(|| AdminSocketPath::default(dirs));

            AdminClient::connect(socket_path.as_ref(), timeout.into())
                .await
                .map(Some)
                .wrap_err("error connecting to admin socket")
                .suggestion("ensure that the NovelNote server is running")
        } else {
            Ok(None)
        }
    }
}

/// Admin socket connection path.
///
/// Must start with `\\.\pipe\` on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdminSocketPath(Box<Path>);

impl AdminSocketPath {
    /// Required prefix for named pipe namespace on Windows.
    #[cfg(windows)]
    const PIPE_PREFIX: &str = r"\\.\pipe\";

    /// Default path for the admin socket.
    fn default(dirs: &ProjectDirs) -> Self {
        #[cfg(windows)]
        let path = {
            let _ = dirs;
            Path::new(Self::PIPE_PREFIX).join("NovelNote_Admin")
        };

        #[cfg(not(windows))]
        let path = dirs
            .runtime_dir()
            .or_else(|| {
                dirs.state_dir()
                    .inspect(|_| tracing::warn!("`XDG_RUNTIME_DIR` not set, using state directory"))
            })
            .unwrap_or_else(|| dirs.data_local_dir())
            .join("novelnote_admin.sock");

        Self(path.into_boxed_path())
    }

    /// Create directory the socket path is in if it does not exist.
    fn create_dir(&self) -> Result<(), Report> {
        if cfg!(unix)
            && let Some(path) = self.0.parent()
            && !path.is_dir()
        {
            std::fs::create_dir_all(path).wrap_err_with(|| {
                format!("error creating socket path directory `{}`", path.display())
            })?;
            info!(socket_path_directory = %path.display(), "created socket path directory");
        }
        Ok(())
    }
}

impl TryFrom<Box<Path>> for AdminSocketPath {
    type Error = Report;

    fn try_from(value: Box<Path>) -> Result<Self, Self::Error> {
        #[cfg(windows)]
        ensure!(
            value.starts_with(Self::PIPE_PREFIX),
            "admin socket path must start with `{}` on Windows",
            Self::PIPE_PREFIX
        );

        Ok(Self(value))
    }
}

impl TryFrom<PathBuf> for AdminSocketPath {
    type Error = Report;

    fn try_from(value: PathBuf) -> Result<Self, Self::Error> {
        value.into_boxed_path().try_into()
    }
}

impl FromStr for AdminSocketPath {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PathBuf::from(s).try_into()
    }
}

impl<'de> Deserialize<'de> for AdminSocketPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PathBuf::deserialize(deserializer)?
            .try_into()
            .map_err(de::Error::custom)
    }
}

impl AsRef<Path> for AdminSocketPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// How long a connection should be open before timing out.
///
/// `S` is max number of seconds a timeout can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timeout<const S: u64>(Duration);

impl<const S: u64> Timeout<S> {
    /// Create a new timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the `duration` is longer than `S` seconds.
    pub(crate) fn new(duration: Duration) -> Result<Self, Report> {
        ensure!(
            duration <= Duration::from_secs(S),
            "timeout must be less than {S} seconds long"
        );
        Ok(Self(duration))
    }
}

impl<const S: u64> TryFrom<Duration> for Timeout<S> {
    type Error = Report;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const S: u64> TryFrom<Span> for Timeout<S> {
    type Error = Report;

    fn try_from(value: Span) -> Result<Self, Self::Error> {
        Duration::try_from(value)?.try_into()
    }
}

impl<'de, const S: u64> Deserialize<'de> for Timeout<S> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Span::deserialize(deserializer)?
            .try_into()
            .map_err(de::Error::custom)
    }
}

impl<const S: u64> FromStr for Timeout<S> {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Span::from_str(s)?.try_into()
    }
}

impl<const S: u64> From<Timeout<S>> for Duration {
    fn from(value: Timeout<S>) -> Self {
        value.0
    }
}

/// Logging configuration. The `[log]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
#[config(layer_attr(command(next_help_heading = "Log Options")))]
pub(crate) struct LogConfig {
    /// Where logs should be written to.
    ///
    /// Can be one of `stdout` (default), `stdout-no-timestamp`, `journald`, `file`, or `none`.
    #[config(
        env = "LOG_OUTPUT",
        default = "stdout",
        layer_attr(arg(short = 'o', long = "log-output", env = "LOG_OUTPUT", value_enum))
    )]
    pub output: LogOutput,

    /// Filter what is logged.
    ///
    /// Each directive can optionally contain a target, span, field(s) (with or without a value),
    /// and/or level formatted like so: `target[span{field=value}]=level`. For example, `debug` will
    /// enable all spans and events at the `debug` level or above. Adding a target,
    /// `novelnote=debug`, limits the spans and events to those created by NovelNote directly.
    ///
    /// See the [`tracing_subscriber::EnvFilter`] documentation for a full description of the
    /// syntax.
    ///
    /// [`tracing_subscriber::EnvFilter`]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives
    #[config(
        env = "LOG_DIRECTIVES",
        default = ["info"],
        layer_attr(arg(
            short = 'l',
            long = "log-directive",
            env = "LOG_DIRECTIVES",
            visible_aliases = ["log-filter", "log-level"],
            value_delimiter = ',',
        ))
    )]
    pub directives: Vec<LogDirective>,

    /// Path to the directory to store the rolling log files in for the `file` log output mode.
    ///
    /// If not set, the following directories are used on each platform:
    ///
    /// - `${XDG_STATE_HOME:$HOME/.local/state}/novelnote` (Linux)
    /// - `$HOME/Library/Application Support/NovelNote` (macOS)
    /// - `%RoamingAppData%/NovelNote/data` (Windows)
    ///
    /// If the directory does not exist, it is created.
    #[config(
        env = "LOG_FILE_DIRECTORY",
        layer_attr(arg(
            long = "log-file-directory",
            env = "LOG_FILE_DIRECTORY",
            visible_aliases = ["log-directory", "log-file-dir", "log-dir"],
            value_name = "LOG_DIR",
            verbatim_doc_comment,
        ))
    )]
    pub file_directory: Option<PathBuf>,

    /// Number of log files to keep.
    ///
    /// The `file` log output mode rotates which file it writes to daily. This option determines how
    /// many log files exist in the log file directory at any given time.
    #[config(env = "MAX_LOG_FILES", default = 3, layer_attr(arg(long, env)))]
    pub max_log_files: usize,
}

/// A [`Directive`] wrapper implementing [`Deserialize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogDirective(Directive);

impl<'de> Deserialize<'de> for LogDirective {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl FromStr for LogDirective {
    type Err = <Directive as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl LogConfig {
    /// Initialize logging by setting a default [`tracing::Subscriber`].
    ///
    /// When writing to log files, a guard which ensures the write buffers are flushed on drop is
    /// returned.
    ///
    /// # Errors
    ///
    /// Returns an error if a global subscriber was already installed.
    pub(crate) fn init_logging(
        &self,
        timezone: TimeZone,
        dirs: &ProjectDirs,
    ) -> Result<Option<WorkerGuard>, Report> {
        let Self {
            output,
            directives,
            file_directory,
            max_log_files,
        } = self;

        let mut created_log_dir = None;
        let (stdout, stdout_no_timestamp, journald, file, guard) = match output {
            LogOutput::Stdout => (
                Some(tracing_subscriber::fmt::layer().with_timer(ZonedTime { timezone })),
                None,
                None,
                None,
                None,
            ),
            LogOutput::StdoutNoTimestamp => (
                None,
                Some(tracing_subscriber::fmt::layer().without_time()),
                None,
                None,
                None,
            ),
            LogOutput::Journald => (
                None,
                None,
                Some(tracing_journald::layer().wrap_err("error connecting to journald socket")?),
                None,
                None,
            ),
            LogOutput::File => {
                let log_dir = file_directory
                    .as_deref()
                    .or_else(|| dirs.state_dir())
                    .unwrap_or_else(|| dirs.data_dir());

                if !log_dir.is_dir() {
                    std::fs::create_dir_all(log_dir).wrap_err_with(|| {
                        format!("error creating log file directory `{}`", log_dir.display())
                    })?;
                    created_log_dir = Some(log_dir);
                }

                let file_appender = RollingFileAppender::builder()
                    .rotation(Rotation::DAILY)
                    .filename_prefix(env!("CARGO_PKG_NAME"))
                    .filename_suffix("log")
                    .max_log_files(*max_log_files)
                    .build(log_dir)
                    .wrap_err("error initializing log file appender")?;

                let (writer, guard) = tracing_appender::non_blocking(file_appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_timer(ZonedTime { timezone })
                    .with_writer(writer);

                (None, None, None, Some(layer), Some(guard))
            }
            LogOutput::None => (None, None, None, None, None),
        };

        let env_filter = directives.clone().into_iter().fold(
            EnvFilter::default(),
            |env_filter, LogDirective(directive)| env_filter.add_directive(directive),
        );

        tracing_subscriber::registry()
            .with(stdout)
            .with(stdout_no_timestamp)
            .with(journald)
            .with(file)
            .with(env_filter)
            .with(ErrorLayer::default())
            .try_init()
            .wrap_err("error initializing tracing subscriber")?;

        debug!("logging enabled");
        if let Some(log_dir) = created_log_dir {
            info!(log_dir = %log_dir.display(), "created log file directory");
        }

        Ok(guard)
    }
}

/// A [`FormatTime`] implementation using [`jiff::Zoned`].
///
/// The current time is rounded to the nearest microsecond.
/// Seconds are written with six decimals of precision for consistent widths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZonedTime {
    /// The time zone to write times in.
    timezone: TimeZone,
}

impl FormatTime for ZonedTime {
    fn format_time(&self, w: &mut Writer) -> std::fmt::Result {
        let now = Timestamp::now()
            .to_zoned(self.timezone.clone())
            .round(Unit::Microsecond)
            .expect("microseconds round cleanly");
        write!(w, "{now:.6}")
    }
}

/// Where logs should be written to.
#[derive(ValueEnum, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LogOutput {
    /// Write logs to stdout, the default.
    #[default]
    Stdout,

    /// Write logs to stdout, without writing a timestamp.
    StdoutNoTimestamp,

    /// Native logging to journald.
    Journald,

    /// Write logs to a rolling set of files, rotating daily.
    File,

    /// Disable logging.
    None,
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
    /// IP address the HTTP server should bind to.
    ///
    /// Defaults to binding to all interfaces.
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
    pub bind_address: IpAddr,

    /// TCP port the HTTP server should bind to.
    #[config(
        env = "HTTP_PORT",
        default = 8080,
        layer_attr(arg(short, long, env = "HTTP_PORT", visible_alias = "http-port"))
    )]
    pub port: u16,

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
    pub(crate) async fn into_server(self, database: Database) -> Server {
        let Self {
            bind_address,
            port,
            asset_directory,
        } = self;

        Server {
            socket_address: SocketAddr::new(bind_address, port),
            asset_directory: if let Some(asset_directory) = asset_directory {
                asset_directory
            } else {
                default_asset_directory().await
            },
            database,
        }
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
