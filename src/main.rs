//! NovelNote is a self-hosted book tracker. Use it to keep track of books you have read and those
//! you want to read.
//!
//! See the project's README for more information.

use color_eyre::eyre::{Report, WrapErr};
use jiff::{Timestamp, Unit, tz::TimeZone};
use tracing::{Level, debug, info};
use tracing_subscriber::{
    EnvFilter,
    fmt::{format, time::FormatTime},
    util::SubscriberInitExt,
};

fn main() -> Result<(), Report> {
    color_eyre::install()?;

    init_logging(TimeZone::system()).wrap_err("error initializing logging")?;

    info!("exiting");
    Ok(())
}

/// Initialize logging by setting a default [`tracing::Subscriber`].
///
/// # Errors
///
/// Returns an error if a global subscriber was already installed.
fn init_logging(time_zone: TimeZone) -> Result<(), Report> {
    tracing_subscriber::fmt()
        .with_timer(ZonedTime { time_zone })
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(Level::INFO.into())
                .from_env_lossy(),
        )
        .finish()
        .try_init()
        .wrap_err("error initializing tracing subscriber")?;

    debug!("logging enabled");
    Ok(())
}

/// A [`FormatTime`] implementation using [`jiff::Zoned`].
///
/// The current time is rounded to the nearest microsecond.
/// Seconds are written with six decimals of precision for consistent widths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZonedTime {
    /// The time zone to write times in.
    time_zone: TimeZone,
}

impl FormatTime for ZonedTime {
    fn format_time(&self, w: &mut format::Writer) -> std::fmt::Result {
        let now = Timestamp::now()
            .to_zoned(self.time_zone.clone())
            .round(Unit::Microsecond)
            .expect("microseconds round cleanly");
        write!(w, "{now:.6}")
    }
}
