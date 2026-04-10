//! NovelNote is a self-hosted book tracker. Use it to keep track of books you have read and those
//! you want to read.
//!
//! See the project's README for more information.

mod config;

use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use color_eyre::{
    Section,
    eyre::{OptionExt, Report, WrapErr},
};
use directories::ProjectDirs;
use tokio::{select, signal};
use tracing::{debug, info, instrument};
use tracing_appender::non_blocking::WorkerGuard;

use crate::config::Config;

fn main() -> Result<(), Report> {
    color_eyre::install()?;

    let _guard = Cli::parse().command.run()?;

    info!("exiting");
    Ok(())
}

/// The command-line interface of `novelnote`.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(version, author, about, long_about = None)]
pub(crate) struct Cli {
    /// Command to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// The `novelnote` command to execute.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Start the NovelNote web server.
    Serve {
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

        /// Layered configuration options.
        #[command(flatten)]
        config: <Config as confique::Config>::Layer,
    },

    /// Generate a sample config file to use with `novelnote serve`.
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
    fn run(self) -> Result<Option<WorkerGuard>, Report> {
        match self {
            Self::Serve {
                config_file,
                config,
            } => {
                let dirs = ProjectDirs::from("", "", "NovelNote")
                    .ok_or_eyre("could not determine home directory")?;

                let config =
                    Config::load(config, config_file, &dirs).wrap_err("error loading config")?;

                let guard = config
                    .log
                    .init_logging(&dirs)
                    .wrap_err("error initializing logging")?;

                debug!(?config, "config loaded");

                start_server(config, &dirs)?;

                Ok(guard)
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

/// Start the HTTP server.
#[tokio::main]
#[instrument(skip_all)]
async fn start_server(
    Config {
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

    info!("starting HTTP server");
    tokio::spawn(http.into_server(database.clone()).run(shutdown_signal()))
        .await
        .wrap_err("HTTP server panicked")?
        .wrap_err("error with HTTP server")?;

    database.close().await.wrap_err("error closing database")
}

/// A [`Future`] which completes when `SIGINT` or `SIGTERM` is received.
#[instrument(level = "debug")]
async fn shutdown_signal() {
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
