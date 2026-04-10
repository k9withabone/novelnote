//! The [`Config`] file format and CLI args for NovelNote.

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, ValueEnum};
use color_eyre::eyre::{Report, WrapErr};
use confique::{Config as _, toml::FormatOptions};
use directories::ProjectDirs;
use jiff::{Timestamp, Unit, fmt::serde::tz, tz::TimeZone};
use novelnote_database::Database;
use novelnote_server::Server;
use serde::{
    Deserialize, Deserializer,
    de::{self, IntoDeserializer},
};
use tokio::task::spawn_blocking;
use tracing::{Span, debug, info, instrument};
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

/// NovelNote configuration.
///
/// Use with `novelnote serve --config-file <path>`.
///
/// See `novelnote serve --help` for default config file locations and additional explanations of
/// enumerated options.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
pub(crate) struct Config {
    /// Logging configuration.
    #[config(
        nested,
        layer_attr(command(flatten, next_help_heading = "Log Options"))
    )]
    pub log: LogConfig,

    /// Database configuration.
    #[config(
        nested,
        layer_attr(command(flatten, next_help_heading = "Database Options"))
    )]
    pub database: DatabaseConfig,

    /// HTTP server configuration.
    #[config(
        nested,
        layer_attr(command(flatten, next_help_heading = "HTTP Server Options"))
    )]
    pub http: HttpServerConfig,
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
        // Environment variable config is handled by clap.
        let mut builder = Self::builder().preloaded(cli_args);

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
}

/// Logging configuration. The `[log]` section in a config file.
#[derive(confique::Config, Debug, Clone, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, PartialEq, Eq)))]
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

    /// Timezone to use when writing timestamps in the log output.
    ///
    /// Used by the `stdout` and `file` log output mode.
    ///
    /// If not set, the system timezone is used if it can be determined.
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

/// Parse [`TimeZone`] from a string.
///
/// Used for parsing the `--timezone` CLI arg in [`LogConfig`].
fn timezone_value_parser(value: &str) -> Result<TimeZone, de::value::Error> {
    tz::required::deserialize(value.into_deserializer())
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
    pub(crate) fn init_logging(&self, dirs: &ProjectDirs) -> Result<Option<WorkerGuard>, Report> {
        let Self {
            output,
            timezone,
            directives,
            file_directory,
            max_log_files,
        } = self;

        let mut created_log_dir = None;
        let (stdout, stdout_no_timestamp, journald, file, guard) = match output {
            LogOutput::Stdout => (
                Some(tracing_subscriber::fmt::layer().with_timer(ZonedTime::new(timezone.clone()))),
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
                    .with_timer(ZonedTime::new(timezone.clone()))
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

impl ZonedTime {
    /// Created a new [`ZonedTime`] from an optional `timezone`.
    ///
    /// If a timezone is not provided, the system timezone is used if it can be determined.
    fn new(timezone: Option<TimeZone>) -> Self {
        Self {
            timezone: timezone.unwrap_or_else(TimeZone::system),
        }
    }
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
    #[config(
        env = "DATABASE_DIRECTORY",
        layer_attr(arg(
            long = "database-directory",
            env = "DATABASE_DIRECTORY",
            visible_aliases = ["database-dir", "db-directory", "db-dir"],
            value_name = "DB_DIR",
            verbatim_doc_comment,
        ))
    )]
    pub directory: Option<PathBuf>,
}

impl DatabaseConfig {
    /// Open a SQLite database in the configured directory.
    #[instrument(name = "open_database", skip_all)]
    pub(crate) async fn open(&self, dirs: &ProjectDirs) -> Result<Database, Report> {
        let Self { directory } = self;
        let directory = directory.as_deref().unwrap_or_else(|| dirs.data_dir());
        {
            let span = Span::current();
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

/// HTTP server configuration. The `[http]` section in a config file.
#[derive(confique::Config, Debug, Clone, Copy, PartialEq, Eq)]
#[config(layer_attr(derive(Args, Debug, Clone, Copy, PartialEq, Eq)))]
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
        layer_attr(arg(short, long, visible_alias = "http-port"))
    )]
    pub port: u16,
}

impl HttpServerConfig {
    /// Create [`Server`] from config.
    pub(crate) const fn into_server(self, database: Database) -> Server {
        let Self { bind_address, port } = self;

        Server {
            socket_address: SocketAddr::new(bind_address, port),
            database,
        }
    }
}
