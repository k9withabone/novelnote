//! NovelNote is a self-hosted book tracker. Use it to keep track of books you have read and those
//! you want to read.
//!
//! See the project's README for more information.

mod config;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{OptionExt, Report, WrapErr};
use directories::ProjectDirs;
use tracing::{debug, info};

use crate::config::Config;

fn main() -> Result<(), Report> {
    color_eyre::install()?;

    let Cli {
        command: Command::Serve {
            config_file,
            config,
        },
    } = Cli::parse();

    let dirs =
        ProjectDirs::from("", "", "NovelNote").ok_or_eyre("could not determine home directory")?;

    let config = Config::load(config, config_file, &dirs).wrap_err("error loading config")?;

    config
        .log
        .init_logging()
        .wrap_err("error initializing logging")?;

    debug!(?config);

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
        /// You can view the final configuration in the debug log output.
        #[arg(short, long, env, verbatim_doc_comment)]
        config_file: Option<PathBuf>,

        /// Layered configuration options.
        #[command(flatten)]
        config: <Config as confique::Config>::Layer,
    },
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
