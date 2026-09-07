//! NovelNote is a self-hosted book tracker. Use it to keep track of books you have read and those
//! you want to read.
//!
//! See the project's README for more information.

mod config;

use std::{
    fs::File,
    io::{self, Write},
    num::NonZero,
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use color_eyre::{
    Section,
    eyre::{OptionExt, Report, WrapErr, eyre},
};
use directories::ProjectDirs;
use jiff::{Timestamp, Unit, civil::DateTime, tz::TimeZone};
use novelnote_admin::AdminClient;
use tokio::{select, signal, try_join};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};
use tracing_appender::non_blocking::WorkerGuard;

use crate::config::{AdminConfig, Config, DatabaseConfig, timezone_value_parser};

/// Filename prefix used for database backups.
const BACKUP_PREFIX: &str = "novelnote_backup_";

fn main() -> Result<(), Report> {
    color_eyre::install()?;

    let _guard = Cli::parse().run()?;

    info!("exiting");
    Ok(())
}

/// The command-line interface of `novelnote`.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(version, author, about, long_about = None)]
struct Cli {
    /// Path to the file to use as the base configuration.
    ///
    /// If not set, a "layered" approach is used, combining the following files in order of descending priority:
    ///
    /// - `novelnote.toml` (in the current working directory)
    /// - `${XDG_CONFIG_DIR:$HOME/.config}/novelnote/novelnote.toml` (Linux)
    /// - `$HOME/Library/Application Support/NovelNote/novelnote.toml` (macOS)
    /// - `%RoamingAppData%/NovelNote/config/novelnote.toml` (Windows)
    /// - `/etc/novelnote/novelnote.toml` (Linux and macOS)
    ///
    /// After loading the specified config file or the above combined config, environment variables and CLI arguments are layered in to produce the final configuration.
    ///
    /// Create a config file template with `novelnote config-template`, which includes the default values for all options.
    ///
    /// You can view the final configuration in the debug log output.
    #[arg(short, long, env, verbatim_doc_comment)]
    config_file: Option<PathBuf>,

    /// Command to execute.
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Execute the command.
    ///
    /// When writing to log files, a guard which ensures the write buffers are flushed on drop is
    /// returned.
    fn run(self) -> Result<Option<WorkerGuard>, Report> {
        let Self {
            config_file,
            command,
        } = self;

        let dirs = ProjectDirs::from("", "", "NovelNote")
            .ok_or_eyre("could not determine home directory")?;

        command.run(config_file, &dirs)
    }
}

/// The `novelnote` command to execute.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Start the NovelNote server.
    ///
    /// In addition to the HTTP server, an admin server is started. It is bound to a Unix domain
    /// socket (Linux and macOS) or named pipe (Windows) that other commands, e.g.
    /// `novelnote backup`, use to communicate with the main process.
    Serve {
        /// Configuration options.
        #[command(flatten)]
        config: <Config as confique::Config>::Layer,
    },

    /// Check to see if the server is running and the database connection is open.
    ///
    /// Requires the NovelNote admin socket to be enabled.
    HealthCheck {
        /// Admin socket configuration options.
        #[command(flatten)]
        admin_config: <AdminConfig as confique::Config>::Layer,
    },

    /// Backup the database.
    ///
    /// Requires the NovelNote admin socket to be enabled.
    Backup(#[command(flatten)] BackupArgs),

    /// Generate a sample config file to use with `novelnote --config-file`.
    ConfigTemplate {
        /// Whether to overwrite any existing file at the given path.
        #[arg(long, alias = "override")]
        overwrite: bool,

        /// Path to write the config file to.
        #[arg(default_value = "novelnote.toml")]
        path: PathBuf,
    },
}

impl Command {
    /// Execute the command.
    ///
    /// When writing to log files, a guard which ensures the write buffers are flushed on drop is
    /// returned.
    fn run(
        self,
        config_file: Option<PathBuf>,
        dirs: &ProjectDirs,
    ) -> Result<Option<WorkerGuard>, Report> {
        match self {
            Self::Serve { config } => {
                let config =
                    Config::load(config, config_file, dirs).wrap_err("error loading config")?;

                let timezone = config.timezone.clone().unwrap_or_else(TimeZone::system);
                let guard = config
                    .log
                    .init_logging(timezone, dirs)
                    .wrap_err("error initializing logging")?;

                debug!(?config, "config loaded");

                run_servers(config, dirs)?;

                Ok(guard)
            }

            Self::HealthCheck { admin_config } => {
                let config = AdminConfig::load(admin_config, config_file, dirs)
                    .wrap_err("error loading admin config")?;

                with_admin_client(config, dirs, async |mut client| {
                    client
                        .health_check()
                        .await
                        .wrap_err("error performing health check")
                })?;

                Ok(None)
            }

            Self::Backup(args) => {
                args.backup(config_file, dirs)?;
                Ok(None)
            }

            #[expect(clippy::print_stdout, reason = "no logging")]
            Self::ConfigTemplate { overwrite, path } => {
                open_file(&path, overwrite)
                    .wrap_err_with(|| {
                        format!(
                            "error opening file `{}` for writing the config template",
                            path.display()
                        )
                    })?
                    .write_all(Config::template().as_bytes())
                    .wrap_err_with(|| {
                        format!("error writing config template to file `{}`", path.display())
                    })?;

                println!("Wrote config template to file `{}`", path.display());
                Ok(None)
            }
        }
    }
}

/// Options for the `novelnote backup` [`Command`].
#[derive(Args, Debug, Clone, PartialEq, Eq)]
struct BackupArgs {
    /// Timezone to use when determining the timestamp for the backup filename.
    ///
    /// By default, the system timezone is used if it can be determined.
    #[arg(
        long,
        env = "TZ",
        visible_alias = "tz",
        value_parser = timezone_value_parser,
        allow_hyphen_values = true,
    )]
    timezone: Option<TimeZone>,

    /// Admin socket configuration options.
    #[command(flatten)]
    admin: <AdminConfig as confique::Config>::Layer,

    /// Database configuration options.
    #[command(flatten)]
    database: <DatabaseConfig as confique::Config>::Layer,
}

impl BackupArgs {
    /// Perform a backup using the configuration and remove old backups.
    ///
    /// # Errors
    ///
    /// Returns an error if loading the config fails, the backup directory path isn't UTF-8,
    /// creating the backup directory fails, creating the backup fails, or there is an error
    /// removing old backups.
    #[expect(clippy::print_stdout, reason = "no logging")]
    fn backup(self, config_file: Option<PathBuf>, dirs: &ProjectDirs) -> Result<(), Report> {
        let Config {
            timezone,
            admin,
            log: _,
            database,
            http: _,
        } = Config::load(self.into(), config_file, dirs).wrap_err("error loading config")?;

        let timezone = timezone.unwrap_or_else(TimeZone::system);
        let now = Timestamp::now()
            .to_zoned(timezone)
            .datetime()
            .round(Unit::Second)
            .expect("seconds round cleanly");
        let now = format!("{now:.0}").replace(':', "");

        let backup_dir = database.backup_directory(dirs);

        #[expect(clippy::map_err_ignore, reason = "using `backup_dir`")]
        let backup_path = backup_dir
            .join(format!("{BACKUP_PREFIX}{now}.sqlite3"))
            .into_os_string()
            .into_string()
            .map_err(|_| {
                eyre!(
                    "backup directory must be UTF-8, backup dir: `{}`",
                    backup_dir.display()
                )
            })?;

        if !backup_dir.is_dir() {
            std::fs::create_dir_all(&backup_dir)
                .wrap_err("error creating database backup directory")?;
            println!(
                "Created database backup directory: `{}`",
                backup_dir.display()
            );
        }

        with_admin_client(admin, dirs, async move |mut client| {
            client
                .backup(backup_path.clone())
                .await
                .wrap_err_with(|| format!("error creating database backup at `{backup_path}`"))?;
            println!("Created backup: `{backup_path}`");
            Ok(())
        })?;

        remove_old_backups(&backup_dir, database.backup.keep_last)
    }
}

/// Remove backups older than the last `keep_last` backups.
///
/// # Errors
///
/// Returns an error if the backup directory cannot be read or removing an old backup fails.
fn remove_old_backups(backup_dir: &Path, keep_last: NonZero<u8>) -> Result<(), Report> {
    // Read the contents of the backup directory, filtering for filenames matching the backup
    // filename format.
    let mut backups = std::fs::read_dir(backup_dir)
        .wrap_err_with(|| format!("error reading backup directory: `{}`", backup_dir.display()))?
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(file_type) if file_type.is_file() => {
                    let backup_date_time: DateTime = entry
                        .file_name()
                        .to_string_lossy()
                        .strip_prefix(BACKUP_PREFIX)?
                        .strip_suffix(".sqlite3")?
                        .parse()
                        .ok()?;
                    Some(Ok((entry.path(), backup_date_time)))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            },
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()
        .wrap_err_with(|| {
            format!(
                "error reading database backup directory: `{}`",
                backup_dir.display()
            )
        })?;

    // Sort backups from newest to oldest.
    #[expect(clippy::min_ident_chars, reason = "comparison")]
    backups.sort_unstable_by(|(_, a), (_, b)| b.cmp(a));

    #[expect(clippy::print_stdout, reason = "no logging")]
    for (path, _) in backups.into_iter().skip(u8::from(keep_last).into()) {
        std::fs::remove_file(&path)
            .wrap_err_with(|| format!("error removing old backup `{}`", path.display()))?;
        println!("Removed old backup `{}`", path.display());
    }

    Ok(())
}

/// Run the HTTP and admin servers.
#[tokio::main]
#[instrument(skip_all)]
async fn run_servers(
    Config {
        timezone: _,
        admin,
        log: _,
        database,
        http,
    }: Config,
    dirs: &ProjectDirs,
) -> Result<(), Report> {
    let database = database
        .open(dirs)
        .await
        .wrap_err("error opening database")?;
    let http_server = http.into_server(database.clone()).await;

    let cancellation_token = CancellationToken::new();
    tokio::spawn(shutdown_signal(cancellation_token.clone()));

    let child_token = cancellation_token.child_token();
    let http_server = async {
        info!("starting HTTP server");
        tokio::spawn(http_server.run(child_token.cancelled_owned()))
            .await
            .wrap_err("HTTP server panicked")?
            .wrap_err("error with HTTP server")
    };

    let admin_server = admin.into_server(dirs, database.clone())?;
    let child_token = cancellation_token.child_token();
    let admin_server = async {
        if let Some(admin_server) = admin_server {
            info!("starting admin server");
            tokio::spawn(async move { admin_server.run(&child_token).await })
                .await
                .wrap_err("admin server panicked")
        } else {
            Ok(())
        }
    };

    try_join!(http_server, admin_server)?;

    database.close().await.wrap_err("error closing database")
}

/// A [`Future`] which completes when `SIGINT` or `SIGTERM` is received.
#[instrument(level = "debug", skip_all)]
async fn shutdown_signal(cancellation_token: CancellationToken) {
    let interrupt = async {
        signal::ctrl_c()
            .await
            .expect("error installing SIGINT handler");
    };

    #[cfg(unix)]
    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("error installing SIGTERM handler");

    #[cfg(windows)]
    let mut terminate = signal::windows::ctrl_close().expect("error install CTRL-CLOSE handler");

    select! {
        () = interrupt => info!("SIGINT received, shutting down..."),
        _ = terminate.recv() => info!("SIGTERM received, shutting down..."),
    }

    cancellation_token.cancel();
}

/// Communicate with the admin socket.
#[tokio::main(flavor = "current_thread")]
async fn with_admin_client<F>(config: AdminConfig, dirs: &ProjectDirs, f: F) -> Result<(), Report>
where
    F: AsyncFnOnce(AdminClient) -> Result<(), Report>,
{
    if let Some(client) = config.into_client(dirs).await? {
        f(client).await
    } else {
        Err(eyre!("admin socket is disabled")
            .suggestion("set `admin.enabled = true` in config or use `--enable-admin-socket true`"))
    }
}

/// Open a [`File`] for writing at the given `path`, optionally overwriting any existing file.
///
/// # Errors
///
/// Returns an error if there is an [`io::Error`] opening the file.
fn open_file(path: &Path, overwrite: bool) -> Result<File, Report> {
    File::options()
        .write(true)
        .truncate(true)
        .create_new(!overwrite)
        .create(overwrite)
        .open(path)
        .map_err(|error| {
            let kind = error.kind();
            let report = Report::new(error);

            if kind == io::ErrorKind::AlreadyExists {
                report
                    .wrap_err("file already exists, not overwriting it")
                    .suggestion("use `--overwrite` to overwrite an existing file")
            } else {
                report.wrap_err("failed to create/open file").suggestion(
                    "make sure the directory exists and you have write permissions for the file",
                )
            }
        })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
