//! [`Generate`] subcommand.

use std::{
    fs::File,
    io::{self, IsTerminal, Write, stdout},
    path::{Path, PathBuf},
};

use clap::Subcommand;
use color_eyre::{
    Section,
    eyre::{Report, WrapErr, eyre},
};
use directories::ProjectDirs;
use inquire::{Confirm, InquireError};
use novelnote_server::SecretKey;

use crate::config::{Config, DatabaseConfig};

/// The `novelnote generate` subcommand for generating a file.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub(crate) enum Generate {
    /// Generate a sample config file to use with `novelnote --config-file`.
    ConfigTemplate {
        /// Whether to overwrite any existing file at the given path.
        #[arg(long, alias = "override")]
        overwrite: bool,

        /// Path to write the config file to.
        #[arg(default_value = "novelnote.toml")]
        path: PathBuf,
    },

    /// Generate a minimum sized secret key file to use with `novelnote serve --secret-key-file`.
    SecretKey {
        /// Whether to overwrite an existing secret key.
        ///
        /// A confirmation will be presented before actually overwriting an existing secret key.
        #[arg(long, alias = "override")]
        overwrite: bool,

        /// Path to write the secret key to.
        ///
        /// Use `-` to write to the standard output. Only available when standard output is not a
        /// terminal.
        ///
        /// Defaults to a `secret_key` file in the database directory (as optionally specified in
        /// the config file) if the standard output is a terminal, otherwise the standard output is
        /// written to.
        path: Option<PathBuf>,
    },
}

impl Generate {
    /// Execute the `generate` command.
    pub(crate) fn run(
        self,
        config_file: Option<PathBuf>,
        dirs: &ProjectDirs,
    ) -> Result<(), Report> {
        match self {
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

                #[expect(clippy::print_stdout, reason = "no logging")]
                {
                    println!("Wrote config template to file `{}`", path.display());
                }
            }

            Self::SecretKey { overwrite, path } => {
                let mut stdout = stdout();
                if path.as_deref().is_some_and(|path| path == "-")
                    || (path.is_none() && !stdout.is_terminal())
                {
                    return if stdout.is_terminal() {
                        Err(eyre!(
                            "cannot write secret key to the standard output if it is a terminal"
                        ))
                    } else {
                        stdout
                            .write_all(SecretKey::generate().as_bytes())
                            .wrap_err("error writing secret key to the standard output")
                    };
                }

                let path = if let Some(path) = path {
                    path
                } else {
                    DatabaseConfig::load(config_file, dirs)
                        .wrap_err("error loading database config")?
                        .directory(dirs)
                        .join("secret_key")
                };

                if overwrite && path.is_file() {
                    let confirm = Confirm::new(
                        "Are you sure you want to overwrite an existing secret key?",
                    )
                    .with_default(false)
                    .with_help_message(
                        "Sessions, tokens, and passwords using the existing key will be invalid if \
                         it is overwritten",
                    )
                    .prompt();

                    match confirm {
                        Ok(true) => {}
                        Ok(false)
                        | Err(
                            InquireError::OperationCanceled | InquireError::OperationInterrupted,
                        ) => return Ok(()),
                        Err(error) => {
                            return Err(error).wrap_err("error confirming secret key overwrite");
                        }
                    }
                }

                open_file(&path, overwrite)
                    .wrap_err_with(|| {
                        format!(
                            "error opening file `{}` for writing the secret key",
                            path.display()
                        )
                    })?
                    .write_all(SecretKey::generate().as_bytes())
                    .wrap_err_with(|| {
                        format!("error writing secret key to file `{}`", path.display())
                    })?;

                #[expect(clippy::print_stdout, reason = "no logging")]
                {
                    println!("Wrote secret key to file `{}`", path.display());
                }
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
