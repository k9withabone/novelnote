//! [`User`] and related types.

mod email_address;
mod username;

use std::{
    fmt::{self, Debug, Display, Formatter},
    net::IpAddr,
};

use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;
use zxcvbn::zxcvbn;

use crate::{Name, rkyv_with::TimestampRoundedSecond, serde_with::timestamp_rounded_second};

pub use self::{
    email_address::{
        ArchivedEmailAddress, ArchivedLettreAddress, EmailAddress, EmailAddressResolver,
    },
    username::{ArchivedUsername, InvalidUsernameError, Username, UsernameResolver},
};

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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
    #[serde(default)]
    pub email: Option<EmailAddress>,

    /// The user's display name.
    ///
    /// Shown when the user is logged in and in user lists if set.
    #[serde(default)]
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

/// Request body parameters for the POST `/api/auth/score-password` route.
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ScorePasswordParameters {
    /// Password to score.
    #[cfg_attr(feature = "utoipa", schema(format = Password))]
    pub password: String,

    /// Other user supplied information to check the password for.
    #[serde(default)]
    pub user_inputs: Vec<String>,
}

/// Score of a password's strength based on how many guesses it would take to crack.
#[derive(
    serde_repr::Serialize_repr,
    serde_repr::Deserialize_repr,
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[repr(u8)]
pub enum PasswordScore {
    /// 10^3 guesses or less.
    Zero = 0,
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(
    feature = "utoipa",
    schema(description = "Error returned when a password is scored less than 3.")
)]
#[error("password was too weak, on a 0-4 scale it scored a {score}")]
pub struct WeakPasswordError {
    /// The password's score based on how many guesses it would take to crack.
    pub score: PasswordScore,

    /// What's wrong with the password.
    #[serde(default)]
    pub warning: Option<String>,

    /// Suggestions to improve the password's strength.
    #[serde(default)]
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

/// A session belonging to a logged-in [`User`].
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(
    feature = "utoipa",
    schema(description = "A session belonging to a logged-in user.")
)]
#[rkyv(derive(Debug, PartialEq, Eq))]
pub struct Session {
    /// Last IP address the session was used from.
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(value_type = String, format = "IPv4 or IPv6 address"))]
    pub ip_address: Option<IpAddr>,

    /// `User-Agent` header from the most recent request of the session.
    #[serde(default)]
    pub user_agent: Option<String>,

    /// When the session began.
    ///
    /// Serialized as seconds from the Unix epoch.
    #[serde(with = "timestamp_rounded_second")]
    #[rkyv(with = TimestampRoundedSecond)]
    #[cfg_attr(feature = "utoipa", schema(value_type = i64))]
    pub started: Timestamp,

    /// When the session will expire.
    ///
    /// Serialized as seconds from the Unix epoch.
    #[serde(with = "timestamp_rounded_second")]
    #[rkyv(with = TimestampRoundedSecond)]
    #[cfg_attr(feature = "utoipa", schema(value_type = i64))]
    pub expires: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod user {
        use std::hint::black_box;

        use proptest::{prop_assert_eq, proptest};
        use rkyv::rancor;

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
}
