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

use std::fmt::{self, Debug, Formatter};

use jiff::{Timestamp, Unit};
use rkyv::string::ArchivedString;

pub use crate::{
    name::{ArchivedName, InvalidNameError, Name, NameResolver},
    user::{User, Username},
};

/// A [`String`] whose [`Debug`] representation is `"REDACTED"`.
#[derive(
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Clone,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(format = Password))]
#[rkyv(derive(PartialEq, Eq), compare(PartialEq))]
pub struct Secret(String);

impl Secret {
    /// Create a new [`Secret`].
    #[must_use]
    pub const fn new(secret: String) -> Self {
        Self(secret)
    }

    /// Reveal the secret as a string slice.
    #[must_use]
    pub const fn reveal(&self) -> &str {
        self.0.as_str()
    }

    /// Reveal the secret as a byte slice.
    #[must_use]
    pub const fn reveal_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Return the inner string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Debug for Secret {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_tuple("Secret").field(&"REDACTED").finish()
    }
}

impl Debug for ArchivedSecret {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_tuple("ArchivedSecret").field(&"REDACTED").finish()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl From<ArchivedString> for ArchivedSecret {
    fn from(value: ArchivedString) -> Self {
        Self(value)
    }
}

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
