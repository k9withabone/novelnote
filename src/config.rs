//! The [`Config`] file format and CLI args for NovelNote.

mod admin;
mod http;
mod log;

use std::{
    borrow::Cow,
    io,
    num::NonZero,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::Args;
use color_eyre::eyre::{Report, WrapErr, eyre};
use confique::toml::FormatOptions;
use directories::ProjectDirs;
use jiff::{
    Span, SpanRelativeTo, SpanRound, Unit,
    fmt::{
        friendly::{Designator, Spacing, SpanPrinter},
        serde::tz,
    },
    tz::TimeZone,
};
use novelnote_database::Database;
use serde::{
    Deserialize, Deserializer,
    de::{self, IntoDeserializer},
};
use tokio::task::spawn_blocking;
use tracing::{debug, info, instrument};

use crate::BackupArgs;

pub(crate) use self::{admin::AdminConfig, http::HttpServerConfig, log::LogConfig};

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

/// [`DatabaseConfig`] nested in the `[database]` table.
///
/// Used for deserializing a part of a [`Config`] file.
#[derive(confique::Config)]
struct NestedDatabaseConfig {
    /// Database configuration.
    #[config(nested)]
    database: DatabaseConfig,
}

impl DatabaseConfig {
    /// Load database config from environment variables and config files.
    ///
    /// If a config file path is provided, only that file is read. Otherwise, several OS standard
    /// locations and the current working directory are checked for config files.
    pub(crate) fn load(
        config_file: Option<PathBuf>,
        dirs: &ProjectDirs,
    ) -> Result<Self, confique::Error> {
        let NestedDatabaseConfig { database } =
            load_config(confique::Layer::from_env()?, config_file, dirs)?;
        Ok(database)
    }

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

/// A [`Duration`] that is limited to `S` number of seconds.
///
/// Parsed from the [`jiff` "friendly" format](https://docs.rs/jiff/latest/jiff/fmt/friendly/index.html).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timeout<const S: u64>(Duration);

impl<const S: u64> Timeout<S> {
    /// Maximum duration this timeout can be.
    const MAX_DURATION: Duration = Duration::from_secs(S);

    /// Create a new timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the `duration` is longer than `S` seconds.
    pub(crate) fn new(duration: Duration) -> Result<Self, Report> {
        if duration > Self::MAX_DURATION {
            let printer = SpanPrinter::new()
                .designator(Designator::Verbose)
                .spacing(Spacing::BetweenUnitsAndDesignators);

            let max = i64::try_from(S).map_or_else(
                |_| printer.unsigned_duration_to_string(&Self::MAX_DURATION),
                |seconds| {
                    let span = Span::new()
                        .seconds(seconds)
                        .round(SpanRound::new().largest(Unit::Week).days_are_24_hours())
                        .expect("balancing only");
                    printer.span_to_string(&span)
                },
            );

            Err(eyre!("duration must be less than {max} long"))
        } else {
            Ok(Self(duration))
        }
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
        let value = value.to_duration(SpanRelativeTo::days_are_24_hours())?;
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
