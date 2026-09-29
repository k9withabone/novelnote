//! [`User`] and related types.

use std::{
    borrow::{Borrow, Cow},
    cmp::Ordering,
    fmt::{self, Debug, Display, Formatter},
};

use rkyv::{
    bytecheck::Verify,
    rancor::{Fallible, Source},
};
use thiserror::Error;
use uuid::Uuid;
use zxcvbn::zxcvbn;

pub use crate::rkyv_with::ArchivedLettreAddress;
use crate::{AsStr, Name, rkyv_with::LettreAddress};

/// A NovelNote user.
#[derive(
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
#[rkyv(derive(Debug, PartialEq, Eq), compare(PartialEq))]
pub struct User {
    /// The user's unique identifier.
    #[cfg_attr(
        test,
        proptest(strategy = "proptest::strategy::Strategy::prop_map(\
            proptest::num::u128::ANY, \
            Uuid::from_u128\
        )")
    )]
    pub id: Uuid,

    /// The user's username.
    ///
    /// Used for logging in and user lists if `display_name` is not set.
    pub username: Username,

    /// The user's email address.
    ///
    /// If enabled by the server admin, can be used for password resets and notifications.
    pub email: Option<EmailAddress>,

    /// The user's display name.
    ///
    /// Shown when the user is logged in and in user lists if set.
    pub display_name: Option<Name>,
}

impl User {
    /// Returns [`Some`] only if the user's email address is [`Verified`].
    ///
    /// [`Verified`]: EmailAddress::Verified
    pub fn verified_email(&self) -> Option<&lettre::Address> {
        self.email.as_ref().and_then(EmailAddress::as_verified)
    }
}

impl ArchivedUser {
    /// Returns [`Some`] only if the user's email address is [`Verified`].
    ///
    /// [`Verified`]: ArchivedEmailAddress::Verified
    pub fn verified_email(&self) -> Option<&ArchivedLettreAddress> {
        self.email
            .as_ref()
            .and_then(ArchivedEmailAddress::as_verified)
    }
}

/// A username, the identifier a user logs in with.
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

/// A [`User`]'s email address.
#[derive(
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
#[serde(rename_all = "snake_case", tag = "status", content = "address")]
#[rkyv(derive(Debug, PartialEq, Eq, Hash), compare(PartialEq))]
pub enum EmailAddress {
    /// It is unknown if the email address belongs to the user.
    Unverified(
        #[rkyv(with = LettreAddress)]
        #[cfg_attr(
            test,
            proptest(strategy = "crate::rkyv_with::tests::lettre_address::strategy()")
        )]
        lettre::Address,
    ),

    /// The email address has been confirmed to belong to the user.
    Verified(
        #[rkyv(with = LettreAddress)]
        #[cfg_attr(
            test,
            proptest(strategy = "crate::rkyv_with::tests::lettre_address::strategy()")
        )]
        lettre::Address,
    ),
}

impl EmailAddress {
    /// Construct a new [`EmailAddress`].
    #[must_use]
    pub const fn new(address: lettre::Address, verified: bool) -> Self {
        if verified {
            Self::Verified(address)
        } else {
            Self::Unverified(address)
        }
    }

    /// Get the email address whether or not it is verified.
    #[must_use]
    pub const fn as_address(&self) -> &lettre::Address {
        let (Self::Verified(address) | Self::Unverified(address)) = self;
        address
    }

    /// Email address, whether or not it is verified, as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_address().as_ref()
    }

    /// Returns `true` if the email address is [`Verified`].
    ///
    /// [`Verified`]: EmailAddress::Verified
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    /// Returns [`Some`] if the email address is [`Verified`].
    ///
    /// [`Verified`]: EmailAddress::Verified
    #[must_use]
    pub const fn as_verified(&self) -> Option<&lettre::Address> {
        if let Self::Verified(address) = self {
            Some(address)
        } else {
            None
        }
    }
}

impl AsRef<str> for EmailAddress {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for EmailAddress {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for EmailAddress {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<EmailAddress> for str {
    fn eq(&self, other: &EmailAddress) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for EmailAddress {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<EmailAddress> for &str {
    fn eq(&self, other: &EmailAddress) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for EmailAddress {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<EmailAddress> for String {
    fn eq(&self, other: &EmailAddress) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for EmailAddress {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<EmailAddress> for Box<str> {
    fn eq(&self, other: &EmailAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for EmailAddress {
    fn eq(&self, other: &Cow<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<EmailAddress> for Cow<'_, str> {
    fn eq(&self, other: &EmailAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl ArchivedEmailAddress {
    /// Get the email address whether or not it is verified.
    #[must_use]
    pub const fn as_address(&self) -> &ArchivedLettreAddress {
        let (Self::Verified(address) | Self::Unverified(address)) = self;
        address
    }

    /// Email address, whether or not it is verified, as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_address().as_str()
    }

    /// Returns `true` if the email address is [`Verified`].
    ///
    /// [`Verified`]: ArchivedEmailAddress::Verified
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    /// Returns [`Some`] if the email address is [`Verified`].
    ///
    /// [`Verified`]: ArchivedEmailAddress::Verified
    #[must_use]
    pub const fn as_verified(&self) -> Option<&ArchivedLettreAddress> {
        if let Self::Verified(address) = self {
            Some(address)
        } else {
            None
        }
    }
}

impl AsRef<str> for ArchivedEmailAddress {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ArchivedEmailAddress {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for ArchivedEmailAddress {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<ArchivedEmailAddress> for str {
    fn eq(&self, other: &ArchivedEmailAddress) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for ArchivedEmailAddress {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ArchivedEmailAddress> for &str {
    fn eq(&self, other: &ArchivedEmailAddress) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for ArchivedEmailAddress {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedEmailAddress> for String {
    fn eq(&self, other: &ArchivedEmailAddress) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for ArchivedEmailAddress {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedEmailAddress> for Box<str> {
    fn eq(&self, other: &ArchivedEmailAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for ArchivedEmailAddress {
    fn eq(&self, other: &Cow<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedEmailAddress> for Cow<'_, str> {
    fn eq(&self, other: &ArchivedEmailAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

/// Score a password's strength.
///
/// Passwords are scored based on how many guesses it would take for an attacker to crack it.
/// A password must score at least [`Three`](PasswordScore::Three) (more than 10^8 guesses to crack)
/// for it to be accepted.
///
/// `user_inputs` is used to check if any other user supplied information is included in the
/// password, making it easier to guess.
///
/// # Errors
///
/// Returns an error if the password scores less than [`Three`](PasswordScore::Three).
pub fn score_password(
    password: &str,
    user_inputs: &[&str],
) -> Result<PasswordScore, WeakPasswordError> {
    let entropy = zxcvbn(password, user_inputs);
    if entropy.score() < zxcvbn::Score::Three {
        Err(WeakPasswordError::from_zxcvbn(&entropy))
    } else {
        Ok(PasswordScore::from_zxcvbn(entropy.score()))
    }
}

/// Score of a password's strength based on how many guesses it would take to crack.
#[derive(
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
)]
pub enum PasswordScore {
    /// 10^3 guesses or less.
    Zero,
    /// 10^6 guesses or less.
    One,
    /// 10^8 guesses or less.
    Two,
    /// 10^10 guesses or less.
    Three,
    /// 10^10 guesses or more.
    Four,
}

impl PasswordScore {
    /// Create a [`PasswordScore`] from a [`zxcvbn::Score`].
    const fn from_zxcvbn(score: zxcvbn::Score) -> Self {
        match score {
            zxcvbn::Score::Zero => Self::Zero,
            zxcvbn::Score::One => Self::One,
            zxcvbn::Score::Two => Self::Two,
            zxcvbn::Score::Three => Self::Three,
            zxcvbn::Score::Four | _ => Self::Four,
        }
    }
}

impl From<PasswordScore> for u8 {
    fn from(value: PasswordScore) -> Self {
        match value {
            PasswordScore::Zero => 0,
            PasswordScore::One => 1,
            PasswordScore::Two => 2,
            PasswordScore::Three => 3,
            PasswordScore::Four => 4,
        }
    }
}

impl Display for PasswordScore {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&u8::from(*self), f)
    }
}

/// Error returned when [scoring a password](score_password()) and it is found to be too weak
/// (scored less than [`Three`](PasswordScore::Three)).
#[derive(
    Error,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
)]
#[error("password was too weak, on a 0-4 scale it scored a {score}")]
pub struct WeakPasswordError {
    /// The password's score based on how many guesses it would take to crack.
    pub score: PasswordScore,

    /// What's wrong with the password.
    pub warning: Option<String>,

    /// Suggestions to improve the password's strength.
    pub suggestions: Vec<String>,
}

impl WeakPasswordError {
    /// Create a [`WeakPasswordError`] from a [`zxcvbn::Entropy`].
    fn from_zxcvbn(entropy: &zxcvbn::Entropy) -> Self {
        let (warning, suggestions) = entropy.feedback().map_or_default(|feedback| {
            let warning = feedback.warning().as_ref().map(ToString::to_string);
            let suggestions = feedback
                .suggestions()
                .iter()
                .map(ToString::to_string)
                .collect();
            (warning, suggestions)
        });

        Self {
            score: PasswordScore::from_zxcvbn(entropy.score()),
            warning,
            suggestions,
        }
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

    mod user {
        use super::*;

        proptest! {
            #[test]
            fn json_round_trip(user: User) {
                prop_assert_eq!(
                    serde_json::from_str::<User>(&serde_json::to_string(&user)?)?,
                    user,
                );
            }

            #[test]
            fn rkyv_no_panic(bytes: Vec<u8>) {
                drop(black_box(rkyv::access::<ArchivedUser, rancor::Error>(&bytes)));
            }

            #[test]
            fn rkyv_round_trip(user: User) {
                let bytes = rkyv::to_bytes::<rancor::Error>(&user)?;
                prop_assert_eq!(rkyv::from_bytes::<User, rancor::Error>(&bytes)?, user);
            }
        }
    }

    mod username {
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

    mod email_address {
        use super::*;

        proptest! {
            #[test]
            fn json_round_trip(email: EmailAddress) {
                prop_assert_eq!(
                    serde_json::from_str::<EmailAddress>(&serde_json::to_string(&email)?)?,
                    email,
                );
            }

            #[test]
            fn rkyv_no_panic(bytes: Vec<u8>) {
                drop(black_box(rkyv::access::<ArchivedEmailAddress, rancor::Error>(&bytes)));
            }

            #[test]
            fn rkyv_round_trip(email: EmailAddress) {
                let bytes = rkyv::to_bytes::<rancor::Error>(&email)?;
                prop_assert_eq!(rkyv::from_bytes::<EmailAddress, rancor::Error>(&bytes)?, email);
            }
        }
    }
}
