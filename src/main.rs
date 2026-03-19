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
use tracing::{debug, info};

use crate::config::Config;

fn main() -> Result<(), Report> {
    color_eyre::install()?;

    Cli::parse().command.run()?;

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
    fn run(self) -> Result<(), Report> {
        match self {
            Self::Serve {
                config_file,
                config,
            } => {
                let dirs = ProjectDirs::from("", "", "NovelNote")
                    .ok_or_eyre("could not determine home directory")?;

                let config =
                    Config::load(config, config_file, &dirs).wrap_err("error loading config")?;

                config
                    .log
                    .init_logging()
                    .wrap_err("error initializing logging")?;

                debug!(?config, "config loaded");
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
            }
        }

        Ok(())
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
