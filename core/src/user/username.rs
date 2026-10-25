//! [`Username`] and related types for [`User`](super::User).

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

/// A username, the identifier a [`User`](super::User) logs in with.
///
/// Usernames must:
///
/// - Not be empty.
/// - Only contain ASCII letters (a-z, A-Z), digits (0-9), underscores (_), periods (.), and
///   hyphens (-), with an optional dollar sign ($) at the end.
/// - Not start with a dash (-).
/// - Not be fully numeric.
/// - Not be '.' or '..'.
/// - Be 256 characters or less.
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
#[cfg_attr(
    feature = "utoipa",
    schema(
        min_length = 1,
        max_length = 256,
        pattern = "[a-zA-Z_][a-zA-Z0-9_.-]{0,254}[a-zA-Z0-9_.$-]?",
    )
)]
#[rkyv(derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash), bytecheck(verify))]
pub struct Username(#[rkyv(with = AsStr)] Box<str>);

impl Username {
    /// Attempt to create a new [`Username`], validating the given string.
    ///
    /// # Errors
    ///
    /// Returns an error if the username is empty, is '.' or '..', starts with '-', is longer than
    /// 256 characters, contains characters other than 'a-zA-Z0-9_.-$', contains '$' not at the end,
    /// or is fully numeric.
    pub fn new<T: AsRef<str> + Into<Box<str>>>(username: T) -> Result<Self, InvalidUsernameError> {
        Self::check(username.as_ref())?;
        Ok(Self(username.into()))
    }

    /// Check if a string slice is a valid [`Username`].
    fn check(username: &str) -> Result<(), InvalidUsernameError> {
        if username.is_empty() {
            Err(InvalidUsernameError::Empty)
        } else if username == "." || username == ".." {
            Err(InvalidUsernameError::Directory)
        } else if username == "$" {
            Err(InvalidUsernameError::DollarSign)
        } else if username.starts_with('-') {
            Err(InvalidUsernameError::Start)
        } else if username.len() > 256 {
            Err(InvalidUsernameError::Length(username.len()))
        } else {
            let mut fully_numeric = true;
            for (n, char) in username.chars().enumerate() {
                match char {
                    'a'..='z' | 'A'..='Z' | '_' | '.' | '-' => fully_numeric = false,
                    '0'..='9' => {}
                    '$' if n == username.len() - 1 => fully_numeric = false,
                    '$' => return Err(InvalidUsernameError::DollarSign),
                    invalid => return Err(InvalidUsernameError::Character(invalid)),
                }
            }

            if fully_numeric {
                Err(InvalidUsernameError::Numeric)
            } else {
                Ok(())
            }
        }
    }

    /// [`Username`] as a string slice.
    ///
    /// Convenience method for `as_ref()` to a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

/// Error returned when creating a new [`Username`] fails.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidUsernameError {
    /// Username was empty.
    #[error("usernames cannot be empty")]
    Empty,

    /// Username was '.' or '..'.
    #[error("usernames cannot be '.' or '..'")]
    Directory,

    /// Username started with '-'.
    #[error("usernames cannot start with '-'")]
    Start,

    /// Username was longer than 256 characters.
    #[error("username was {0} characters long, the max is 256 characters")]
    Length(usize),

    /// Username contained '$' not at the end.
    #[error("usernames may only have '$' at the end")]
    DollarSign,

    /// Username contained an invalid character.
    #[error(
        "invalid username character `{0}`, usernames may only contain ASCII letters (a-z, A-Z), \
            digits (0-9), underscores (_), periods (.), and hyphens (-), with an optional \
            dollar sign ($) at the end"
    )]
    Character(char),

    /// Username was fully numeric.
    #[error("usernames cannot be fully numeric")]
    Numeric,
}

impl serde::Serialize for Username {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Username {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<str>::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl FromStr for Username {
    type Err = InvalidUsernameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<Box<str>> for Username {
    type Error = InvalidUsernameError;

    fn try_from(value: Box<str>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Username {
    type Error = InvalidUsernameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Display for Username {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Username {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Username {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Username {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<Username> for str {
    fn eq(&self, other: &Username) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for Username {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Username> for &str {
    fn eq(&self, other: &Username) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for Username {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Username> for String {
    fn eq(&self, other: &Username) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for Username {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<Username> for Box<str> {
    fn eq(&self, other: &Username) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for Username {
    fn eq(&self, other: &Cow<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<Username> for Cow<'_, str> {
    fn eq(&self, other: &Username) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialOrd<str> for Username {
    fn partial_cmp(&self, other: &str) -> Option<Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<Username> for str {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.partial_cmp(other.as_str())
    }
}

impl PartialOrd<&str> for Username {
    fn partial_cmp(&self, other: &&str) -> Option<Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<Username> for &str {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.partial_cmp(&other.as_str())
    }
}

impl PartialOrd<String> for Username {
    fn partial_cmp(&self, other: &String) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Username> for String {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Box<str>> for Username {
    fn partial_cmp(&self, other: &Box<str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<Username> for Box<str> {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Cow<'_, str>> for Username {
    fn partial_cmp(&self, other: &Cow<'_, str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<Username> for Cow<'_, str> {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl From<Username> for Box<str> {
    fn from(value: Username) -> Self {
        value.0
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.0.into_string()
    }
}

impl ArchivedUsername {
    /// Extract the string slice of the username.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

// SAFETY: checks for `Username` validity, which is the only guarantee for `ArchivedUsername`.
#[expect(unsafe_code, reason = "rkyv Verify")]
unsafe impl<C> Verify<C> for ArchivedUsername
where
    C: Fallible + ?Sized,
    C::Error: Source,
{
    fn verify(&self, _context: &mut C) -> Result<(), C::Error> {
        Username::check(self.as_str()).map_err(Source::new)
    }
}

impl AsRef<str> for ArchivedUsername {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl Borrow<str> for ArchivedUsername {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Display for ArchivedUsername {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<Username> for ArchivedUsername {
    fn eq(&self, other: &Username) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedUsername> for Username {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for ArchivedUsername {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<ArchivedUsername> for str {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for ArchivedUsername {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ArchivedUsername> for &str {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for ArchivedUsername {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedUsername> for String {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for ArchivedUsername {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedUsername> for Box<str> {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for ArchivedUsername {
    fn eq(&self, other: &Cow<'_, str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedUsername> for Cow<'_, str> {
    fn eq(&self, other: &ArchivedUsername) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialOrd<Username> for ArchivedUsername {
    fn partial_cmp(&self, other: &Username) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<ArchivedUsername> for Username {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<str> for ArchivedUsername {
    fn partial_cmp(&self, other: &str) -> Option<Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<ArchivedUsername> for str {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        self.partial_cmp(other.as_str())
    }
}

impl PartialOrd<&str> for ArchivedUsername {
    fn partial_cmp(&self, other: &&str) -> Option<Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<ArchivedUsername> for &str {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        (*self).partial_cmp(other.as_str())
    }
}

impl PartialOrd<String> for ArchivedUsername {
    fn partial_cmp(&self, other: &String) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<ArchivedUsername> for String {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Box<str>> for ArchivedUsername {
    fn partial_cmp(&self, other: &Box<str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedUsername> for Box<str> {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Cow<'_, str>> for ArchivedUsername {
    fn partial_cmp(&self, other: &Cow<'_, str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedUsername> for Cow<'_, str> {
    fn partial_cmp(&self, other: &ArchivedUsername) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use proptest::{
        arbitrary::Arbitrary,
        prop_assert_eq, proptest,
        strategy::statics::Map as PropMap,
        string::{RegexGeneratorStrategy, string_regex},
    };
    use rkyv::rancor;

    use super::*;

    impl Arbitrary for Username {
        type Parameters = ();
        type Strategy = PropMap<
            PropMap<RegexGeneratorStrategy<String>, fn(String) -> Box<str>>,
            fn(Box<str>) -> Self,
        >;

        fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
            let regex = string_regex("[a-zA-Z_][a-zA-Z0-9_.-]{0,254}[a-zA-Z0-9_.$-]?")
                .expect("valid regex");
            PropMap::new(PropMap::new(regex, String::into_boxed_str), Self)
        }
    }

    proptest! {
        #[test]
        fn no_panic(string: String) {
            drop(black_box(Username::new(string)));
        }

        #[test]
        fn valid(username: Username) {
            Username::new(username)?;
        }

        #[test]
        fn character_err(username in "[^a-zA-Z0-9_.$-]") {
            let char = username.chars().next().unwrap_or_default();
            prop_assert_eq!(
                Username::new(username),
                Err(InvalidUsernameError::Character(char)),
            );
        }

        #[test]
        fn numeric_err(username in "[0-9]{1,256}") {
            prop_assert_eq!(Username::new(username), Err(InvalidUsernameError::Numeric));
        }

        #[test]
        fn json_round_trip(username: Username) {
            prop_assert_eq!(
                serde_json::from_str::<Username>(&serde_json::to_string(&username)?)?,
                username,
            );
        }

        #[test]
        fn rkyv_no_panic(bytes: Vec<u8>) {
            drop(black_box(rkyv::access::<ArchivedUsername, rancor::Error>(&bytes)));
        }

        #[test]
        fn rkyv_round_trip(username: Username) {
            let bytes = rkyv::to_bytes::<rancor::Error>(&username)?;
            prop_assert_eq!(rkyv::from_bytes::<Username, rancor::Error>(&bytes)?, username);
        }
    }

    #[test]
    fn empty_err() {
        assert_eq!(Username::new(""), Err(InvalidUsernameError::Empty));
    }

    #[test]
    fn directory_err() {
        assert_eq!(Username::new("."), Err(InvalidUsernameError::Directory));
        assert_eq!(Username::new(".."), Err(InvalidUsernameError::Directory));
    }

    #[test]
    fn hyphen_start_err() {
        assert_eq!(Username::new("-test"), Err(InvalidUsernameError::Start));
    }

    #[test]
    fn length_err() {
        let username: String = ['a'; 257].into_iter().collect();
        assert_eq!(
            Username::new(username),
            Err(InvalidUsernameError::Length(257)),
        );
    }

    #[test]
    fn dollar_sign_err() -> Result<(), InvalidUsernameError> {
        Username::new("test$")?;
        assert_eq!(Username::new("te$t"), Err(InvalidUsernameError::DollarSign));
        assert_eq!(Username::new("$"), Err(InvalidUsernameError::DollarSign));
        Ok(())
    }

    #[test]
    fn rkyv_access_err() -> Result<(), rancor::Error> {
        let invalid_username = Username(Box::from(""));
        let bytes = rkyv::to_bytes(&invalid_username)?;
        rkyv::access::<ArchivedUsername, rancor::Error>(&bytes).expect_err("invalid username");

        Ok(())
    }
}
