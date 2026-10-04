//! `novelnote_core` provides shared type definitions for NovelNote packages. NovelNote is a
//! self-hosted book tracker.
//!
//! # Cargo Features
//!
//! - `utoipa`: Enables [`utoipa`] traits like [`ToSchema`](utoipa::ToSchema).

#![cfg_attr(docsrs, feature(doc_cfg))]

mod rkyv_with;
pub mod user;

use std::{
    borrow::{Borrow, Cow},
    cmp::Ordering,
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use rkyv::{
    bytecheck::Verify,
    rancor::{Fallible, Source},
};
use thiserror::Error;

use crate::rkyv_with::AsStr;
pub use crate::user::{User, Username};

/// A name for someone or something.
///
/// Names cannot be empty, contain multiple lines, or start or end with whitespace.
#[derive(
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(min_length = 1, pattern = r"\S+(.*\S+)?"))]
#[rkyv(derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash), bytecheck(verify))]
pub struct Name(#[rkyv(with = AsStr)] Box<str>);

impl Name {
    /// Attempt to create a new [`Name`], validating the given string.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty, contains multiple lines, or starts or ends with
    /// whitespace.
    pub fn new<T: AsRef<str> + Into<Box<str>>>(name: T) -> Result<Self, InvalidNameError> {
        Self::check(name.as_ref())?;
        Ok(Self(name.into()))
    }

    /// Check if a string slice is a valid [`Name`].
    fn check(name: &str) -> Result<(), InvalidNameError> {
        if name.is_empty() {
            Err(InvalidNameError::Empty)
        } else if name.contains('\n') {
            Err(InvalidNameError::MultipleLines)
        } else if name.starts_with(char::is_whitespace) || name.ends_with(char::is_whitespace) {
            Err(InvalidNameError::Whitespace)
        } else {
            Ok(())
        }
    }

    /// [`Name`] as a string slice.
    ///
    /// Convenience method for `as_ref()` to a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

/// Error returned when creating a new [`Name`] fails.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidNameError {
    /// Name was empty.
    #[error("names cannot be empty")]
    Empty,

    /// Name contained a newline ('\n') character.
    #[error("names cannot contain multiple lines")]
    MultipleLines,

    /// Name started or ended with whitespace.
    #[error("names cannot start or end with whitespace")]
    Whitespace,
}

impl serde::Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<str>::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl FromStr for Name {
    type Err = InvalidNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<Box<str>> for Name {
    type Error = InvalidNameError;

    fn try_from(value: Box<str>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Name {
    type Error = InvalidNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Display for Name {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<Name> for str {
    fn eq(&self, other: &Name) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Name> for &str {
    fn eq(&self, other: &Name) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for Name {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Name> for String {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for Name {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<Name> for Box<str> {
    fn eq(&self, other: &Name) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for Name {
    fn eq(&self, other: &Cow<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<Name> for Cow<'_, str> {
    fn eq(&self, other: &Name) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialOrd<str> for Name {
    fn partial_cmp(&self, other: &str) -> Option<Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<Name> for str {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.partial_cmp(other.as_str())
    }
}

impl PartialOrd<&str> for Name {
    fn partial_cmp(&self, other: &&str) -> Option<Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<Name> for &str {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.partial_cmp(&other.as_str())
    }
}

impl PartialOrd<String> for Name {
    fn partial_cmp(&self, other: &String) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Name> for String {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Box<str>> for Name {
    fn partial_cmp(&self, other: &Box<str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<Name> for Box<str> {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Cow<'_, str>> for Name {
    fn partial_cmp(&self, other: &Cow<'_, str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<Name> for Cow<'_, str> {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl From<Name> for Box<str> {
    fn from(value: Name) -> Self {
        value.0
    }
}

impl From<Name> for String {
    fn from(value: Name) -> Self {
        value.0.into_string()
    }
}

impl ArchivedName {
    /// Extract the string slice of the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

// SAFETY: checks for `Name` validity, which is the only guarantee for `ArchivedName`.
#[expect(unsafe_code, reason = "rkyv Verify")]
unsafe impl<C> Verify<C> for ArchivedName
where
    C: Fallible + ?Sized,
    C::Error: Source,
{
    fn verify(&self, _context: &mut C) -> Result<(), C::Error> {
        Name::check(self.as_str()).map_err(Source::new)
    }
}

impl AsRef<str> for ArchivedName {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl Borrow<str> for ArchivedName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Display for ArchivedName {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<Name> for ArchivedName {
    fn eq(&self, other: &Name) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedName> for Name {
    fn eq(&self, other: &ArchivedName) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for ArchivedName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<ArchivedName> for str {
    fn eq(&self, other: &ArchivedName) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for ArchivedName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ArchivedName> for &str {
    fn eq(&self, other: &ArchivedName) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for ArchivedName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedName> for String {
    fn eq(&self, other: &ArchivedName) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for ArchivedName {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedName> for Box<str> {
    fn eq(&self, other: &ArchivedName) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for ArchivedName {
    fn eq(&self, other: &Cow<'_, str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedName> for Cow<'_, str> {
    fn eq(&self, other: &ArchivedName) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialOrd<Name> for ArchivedName {
    fn partial_cmp(&self, other: &Name) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<ArchivedName> for Name {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<str> for ArchivedName {
    fn partial_cmp(&self, other: &str) -> Option<Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<ArchivedName> for str {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        self.partial_cmp(other.as_str())
    }
}

impl PartialOrd<&str> for ArchivedName {
    fn partial_cmp(&self, other: &&str) -> Option<Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<ArchivedName> for &str {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        (*self).partial_cmp(other.as_str())
    }
}

impl PartialOrd<String> for ArchivedName {
    fn partial_cmp(&self, other: &String) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<ArchivedName> for String {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Box<str>> for ArchivedName {
    fn partial_cmp(&self, other: &Box<str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedName> for Box<str> {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Cow<'_, str>> for ArchivedName {
    fn partial_cmp(&self, other: &Cow<'_, str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedName> for Cow<'_, str> {
    fn partial_cmp(&self, other: &ArchivedName) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod name {
        use std::hint::black_box;

        use proptest::{
            arbitrary::Arbitrary,
            prop_assert_eq, proptest,
            strategy::statics::Map,
            string::{RegexGeneratorStrategy, string_regex},
        };
        use rkyv::rancor;

        use super::*;

        impl Arbitrary for Name {
            type Parameters = ();
            type Strategy = Map<
                Map<RegexGeneratorStrategy<String>, fn(String) -> Box<str>>,
                fn(Box<str>) -> Self,
            >;

            fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
                let regex = string_regex(r"\S+(.*\S+)?").expect("valid regex");
                Map::new(Map::new(regex, String::into_boxed_str), Self)
            }
        }

        proptest! {
            #[test]
            fn no_panic(name: String) {
                drop(black_box(Name::new(name)));
            }

            #[test]
            fn valid(name: Name) {
                Name::new(name.0)?;
            }

            #[test]
            fn json_round_trip(name: Name) {
                prop_assert_eq!(
                    serde_json::from_str::<Name>(&serde_json::to_string(&name)?)?,
                    name,
                );
            }

            #[test]
            fn rkyv_no_panic(bytes: Vec<u8>) {
                drop(black_box(rkyv::access::<ArchivedName, rancor::Error>(&bytes)));
            }

            #[test]
            fn rkyv_round_trip(name: Name) {
                let bytes = rkyv::to_bytes::<rancor::Error>(&name)?;
                prop_assert_eq!(rkyv::from_bytes::<Name, rancor::Error>(&bytes)?, name);
            }
        }

        #[test]
        fn empty_err() {
            assert_eq!(Name::new(""), Err(InvalidNameError::Empty));
        }

        #[test]
        fn multiple_lines_err() {
            assert_eq!(
                Name::new("test\ntest"),
                Err(InvalidNameError::MultipleLines),
            );
        }

        #[test]
        fn whitespace_err() {
            assert_eq!(Name::new(" test"), Err(InvalidNameError::Whitespace));
            assert_eq!(Name::new("test "), Err(InvalidNameError::Whitespace));
        }

        #[test]
        fn json_err() {
            assert!(
                serde_json::from_str::<Name>("\"test\\ntest\"")
                    .expect_err("invalid name")
                    .is_data()
            );
        }

        /// Check that invalid [`Name`]s can't be accessed.
        #[test]
        fn rkyv_access_err() -> Result<(), rancor::Error> {
            let invalid_name = Name(Box::from("test\ntest"));
            let bytes = rkyv::to_bytes(&invalid_name)?;

            rkyv::access::<ArchivedName, rancor::Error>(&bytes).expect_err("invalid name");

            Ok(())
        }
    }
}
