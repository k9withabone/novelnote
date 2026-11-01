//! `novelnote_core` provides shared type definitions for NovelNote packages. NovelNote is a
//! self-hosted book tracker.
//!
//! # Cargo Features
//!
//! - `utoipa`: Enables [`utoipa`] traits like [`ToSchema`](utoipa::ToSchema).

#![cfg_attr(docsrs, feature(doc_cfg))]

mod name;
mod rkyv_with;
mod serde_with;
pub mod user;

use jiff::{Timestamp, Unit};

pub use crate::{
    name::{ArchivedName, InvalidNameError, Name, NameResolver},
    user::{User, Username},
};

/// Extension trait for [`Timestamp`].
///
/// This trait is sealed.
#[expect(private_bounds, reason = "sealed trait")]
pub trait TimestampExt: private::Sealed {
    /// Returns this timestamp as a rounded number of seconds since the Unix epoch.
    #[expect(clippy::wrong_self_convention, reason = "Timestamp is Copy")]
    fn as_rounded_second(self) -> i64;
}

impl TimestampExt for Timestamp {
    fn as_rounded_second(self) -> i64 {
        self.round(Unit::Second)
            .expect("round to nearest second cleanly")
            .as_second()
    }
}

/// Private sealed trait for [`TimestampExt`].
mod private {
    use jiff::Timestamp;

    /// Private.
    pub(crate) trait Sealed {}

    impl Sealed for Timestamp {}
}
