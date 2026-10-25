//! [`EmailAddress`] implementation for [`User`](super::User).

use std::borrow::{Borrow, Cow};

pub use crate::rkyv_with::ArchivedLettreAddress;
use crate::rkyv_with::LettreAddress;

/// A [`User`](super::User)'s email address.
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(
    feature = "utoipa",
    schema(examples(json!({"status": "unverified", "address": "user@example.com"}))),
)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
#[serde(rename_all = "snake_case", tag = "status", content = "address")]
#[rkyv(derive(Debug, PartialEq, Eq, Hash), compare(PartialEq))]
pub enum EmailAddress {
    /// It is unknown if the email address belongs to the user.
    #[cfg_attr(feature = "utoipa", schema(value_type = String, format = IdnEmail))]
    Unverified(
        #[rkyv(with = LettreAddress)]
        #[cfg_attr(
            test,
            proptest(strategy = "crate::rkyv_with::tests::lettre_address::strategy()")
        )]
        lettre::Address,
    ),

    /// The email address has been confirmed to belong to the user.
    #[cfg_attr(feature = "utoipa", schema(value_type = String, format = IdnEmail))]
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

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use proptest::{prop_assert_eq, proptest};
    use rkyv::rancor;

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
