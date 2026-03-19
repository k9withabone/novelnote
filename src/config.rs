//! The [`Config`] file format and CLI args for NovelNote.

use std::{path::PathBuf, str::FromStr};

use clap::{Args, ValueEnum};
use color_eyre::eyre::{Report, WrapErr};
use confique::{Config as _, toml::FormatOptions};
use directories::ProjectDirs;
use jiff::{Timestamp, Unit, fmt::serde::tz, tz::TimeZone};
use serde::{
    Deserialize, Deserializer,
    de::{self, IntoDeserializer},
};
use tracing::debug;
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
    /// Can be one of `stdout` (default), `stdout-no-timestamp`, or `none`.
    #[config(
        env = "LOG_OUTPUT",
        default = "stdout",
        layer_attr(arg(short = 'o', long = "log-output", env = "LOG_OUTPUT", value_enum))
    )]
    pub output: LogOutput,

    /// Timezone to use when writing timestamps in the log output.
    ///
    /// Used by the `stdout` log output mode.
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
    /// # Errors
    ///
    /// Returns an error if a global subscriber was already installed.
    pub(crate) fn init_logging(&self) -> Result<(), Report> {
        let Self {
            output,
            timezone,
            directives,
        } = self;

        let (stdout, stdout_no_timestamp) = match output {
            LogOutput::Stdout => (
                Some(tracing_subscriber::fmt::layer().with_timer(ZonedTime {
                    timezone: timezone.clone().unwrap_or_else(TimeZone::system),
                })),
                None,
            ),
            LogOutput::StdoutNoTimestamp => {
                (None, Some(tracing_subscriber::fmt::layer().without_time()))
            }
            LogOutput::None => (None, None),
        };

        let env_filter = directives.clone().into_iter().fold(
            EnvFilter::default(),
            |env_filter, LogDirective(directive)| env_filter.add_directive(directive),
        );

        tracing_subscriber::registry()
            .with(stdout)
            .with(stdout_no_timestamp)
            .with(env_filter)
            .with(ErrorLayer::default())
            .try_init()
            .wrap_err("error initializing tracing subscriber")?;

        debug!("logging enabled");
        Ok(())
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

    /// Disable logging.
    None,
}
