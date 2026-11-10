//! [`AdminConfig`] and related types.

use std::{
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Args;
use color_eyre::{
    Section,
    eyre::{Report, WrapErr},
};
use directories::ProjectDirs;
use novelnote_admin::{AdminClient, AdminServer};
use novelnote_database::Database;
use serde::{Deserialize, Deserializer, de};

#[cfg(unix)]
use super::create_parent_dir;
use super::{Timeout, load_config};

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
    #[cfg_attr(not(unix), expect(clippy::unused_async, reason = "cfg"))]
    pub(crate) async fn try_into_server(
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

            #[cfg(unix)]
            create_parent_dir(socket_path.as_ref())
                .await
                .wrap_err_with(|| {
                    format!("error creating parent directory for admin socket `{socket_path}`")
                })?;

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

impl Display for AdminSocketPath {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&self.0.display(), f)
    }
}
