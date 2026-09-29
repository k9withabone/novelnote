//! Wrapper types to be used in `#[rkyv(with = ..)]`.

use std::{
    borrow::{Borrow, Cow},
    cmp::Ordering,
    fmt::{self, Display, Formatter},
    hash::{Hash, Hasher},
};

use email_address::EmailAddress;
use rkyv::{
    Archive, Deserialize, Place, Portable, SerializeUnsized,
    bytecheck::{CheckBytes, Verify},
    munge::munge,
    primitive::ArchivedUsize,
    rancor::{Fallible, Source},
    string::{ArchivedString, StringResolver},
    with::{ArchiveWith, DeserializeWith, SerializeWith},
};
use thiserror::Error;

/// A wrapper that archives a type to [`ArchivedString`].
///
/// This is similar to [`rkyv::with::AsString`], but all types implementing [`AsRef<str>`] can be
/// serialized and those implementing [`From<String>`] can be deserialized.
#[derive(Debug)]
pub(crate) struct AsStr;

impl<T: AsRef<str> + ?Sized> ArchiveWith<T> for AsStr {
    type Archived = ArchivedString;
    type Resolver = StringResolver;

    fn resolve_with(field: &T, resolver: Self::Resolver, out: Place<Self::Archived>) {
        ArchivedString::resolve_from_str(field.as_ref(), resolver, out);
    }
}

impl<T, S> SerializeWith<T, S> for AsStr
where
    T: AsRef<str> + ?Sized,
    S: Fallible + ?Sized,
    S::Error: Source,
    str: SerializeUnsized<S>,
{
    fn serialize_with(field: &T, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        ArchivedString::serialize_from_str(field.as_ref(), serializer)
    }
}

impl<T, D> DeserializeWith<ArchivedString, T, D> for AsStr
where
    String: Into<T>,
    D: Fallible + ?Sized,
{
    fn deserialize_with(field: &ArchivedString, deserializer: &mut D) -> Result<T, D::Error> {
        field.deserialize(deserializer).map(Into::into)
    }
}

/// A wrapper which archives [`lettre::Address`].
#[derive(Debug)]
pub(crate) struct LettreAddress;

impl ArchiveWith<lettre::Address> for LettreAddress {
    type Archived = ArchivedLettreAddress;
    type Resolver = LettreAddressResolver;

    fn resolve_with(field: &lettre::Address, resolver: Self::Resolver, out: Place<Self::Archived>) {
        let serialized = field.as_ref();
        let at_start = field.user().len().try_into().expect("pointer width is 32");

        munge!(
            let ArchivedLettreAddress {
                serialized: serialized_out,
                at_start: at_start_out,
            } = out
        );
        ArchivedString::resolve_from_str(serialized, resolver.serialized, serialized_out);
        ArchivedUsize::resolve(&at_start, (), at_start_out);
    }
}

impl<S> SerializeWith<lettre::Address, S> for LettreAddress
where
    S: Fallible + ?Sized,
    S::Error: Source,
    str: SerializeUnsized<S>,
{
    fn serialize_with(
        field: &lettre::Address,
        serializer: &mut S,
    ) -> Result<Self::Resolver, S::Error> {
        Ok(LettreAddressResolver {
            serialized: ArchivedString::serialize_from_str(field.as_ref(), serializer)?,
        })
    }
}

impl<D> DeserializeWith<ArchivedLettreAddress, lettre::Address, D> for LettreAddress
where
    D: Fallible + ?Sized,
{
    fn deserialize_with(
        field: &ArchivedLettreAddress,
        _deserializer: &mut D,
    ) -> Result<lettre::Address, D::Error> {
        // DANGER: ArchivedLettreAddress is guaranteed to contain a valid email address (using the
        //         Verify impl).
        Ok(lettre::Address::new_dangerous(field.user(), field.domain()))
    }
}

/// The archived form of [`lettre::Address`].
#[derive(Debug, Portable, CheckBytes)]
#[bytecheck(crate = rkyv::bytecheck, verify)]
#[repr(C)]
pub struct ArchivedLettreAddress {
    /// Archived complete email address.
    serialized: ArchivedString,

    /// Archived index of `serialized` before the '@'.
    at_start: ArchivedUsize,
}

impl ArchivedLettreAddress {
    /// Get the email address from the archive as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.serialized.as_str()
    }

    /// Get the index in the email address where the '@' starts.
    #[must_use]
    fn at_start(&self) -> usize {
        self.at_start
            .try_into()
            .expect("pointer width will always be equal to or less than usize")
    }

    /// Get the local user part of the email address.
    #[expect(clippy::missing_panics_doc, reason = "will not panic")]
    #[must_use]
    pub fn user(&self) -> &str {
        self.as_str()
            .get(..self.at_start())
            .expect("`at_start` guaranteed to be at '@'")
    }

    /// Get the domain part of the email address.
    #[expect(clippy::missing_panics_doc, reason = "will not panic")]
    #[must_use]
    pub fn domain(&self) -> &str {
        self.as_str()
            .get(self.at_start() + 1..)
            .expect("`at_start` guaranteed to be at '@'")
    }
}

// SAFETY: checks for email address validity, first ensuring `at_start` is in the correct place.
#[expect(unsafe_code, reason = "rkyv Verify")]
unsafe impl<C> Verify<C> for ArchivedLettreAddress
where
    C: Fallible + ?Sized,
    C::Error: Source,
{
    fn verify(&self, _context: &mut C) -> Result<(), C::Error> {
        let at_start = self.at_start();
        let at = self
            .as_str()
            .get(at_start..at_start + 1)
            .ok_or_else(|| Source::new(LettreAddressError::AtMisplaced))?;

        if at != "@" {
            Err(Source::new(LettreAddressError::AtMisplaced))
        } else if !EmailAddress::is_valid_local_part(self.user()) {
            Err(Source::new(LettreAddressError::InvalidUser(
                self.user().into(),
            )))
        } else if !EmailAddress::is_valid_domain(self.domain()) {
            Err(Source::new(LettreAddressError::InvalidDomain(
                self.domain().into(),
            )))
        } else {
            Ok(())
        }
    }
}

/// Possible errors when verifying that a [`ArchivedLettreAddress`] is valid fails.
#[derive(Error, Debug)]
enum LettreAddressError {
    /// `at_start` was not at the '@'.
    #[error("`at_start` index was not at '@'")]
    AtMisplaced,

    /// Local user part of the email address was invalid.
    #[error("invalid email user `{0}`")]
    InvalidUser(Box<str>),

    /// Domain part of the email address was invalid.
    #[error("invalid email domain `{0}`")]
    InvalidDomain(Box<str>),
}

/// Resolver for an archived [`lettre::Address`] using the [`LettreAddress`] wrapper.
pub(crate) struct LettreAddressResolver {
    /// Resolver for the `serialized` field in [`ArchivedLettreAddress`].
    serialized: StringResolver,
}

impl AsRef<str> for ArchivedLettreAddress {
    fn as_ref(&self) -> &str {
        self.serialized.as_ref()
    }
}

impl Borrow<str> for ArchivedLettreAddress {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Display for ArchivedLettreAddress {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq for ArchivedLettreAddress {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for ArchivedLettreAddress {}

impl PartialEq<lettre::Address> for ArchivedLettreAddress {
    fn eq(&self, other: &lettre::Address) -> bool {
        self.as_str() == AsRef::<str>::as_ref(&other)
    }
}

impl PartialEq<ArchivedLettreAddress> for lettre::Address {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        AsRef::<str>::as_ref(&self) == other.as_str()
    }
}

impl PartialEq<str> for ArchivedLettreAddress {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<ArchivedLettreAddress> for str {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<&str> for ArchivedLettreAddress {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ArchivedLettreAddress> for &str {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for ArchivedLettreAddress {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<ArchivedLettreAddress> for String {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Box<str>> for ArchivedLettreAddress {
    fn eq(&self, other: &Box<str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedLettreAddress> for Box<str> {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialEq<Cow<'_, str>> for ArchivedLettreAddress {
    fn eq(&self, other: &Cow<'_, str>) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl PartialEq<ArchivedLettreAddress> for Cow<'_, str> {
    fn eq(&self, other: &ArchivedLettreAddress) -> bool {
        self.as_ref() == other.as_str()
    }
}

impl PartialOrd for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ArchivedLettreAddress {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd<lettre::Address> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &lettre::Address) -> Option<Ordering> {
        self.as_str().partial_cmp(AsRef::<str>::as_ref(&other))
    }
}

impl PartialOrd<ArchivedLettreAddress> for lettre::Address {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        AsRef::<str>::as_ref(&self).partial_cmp(other.as_str())
    }
}

impl PartialOrd<str> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &str) -> Option<Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<ArchivedLettreAddress> for str {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        self.partial_cmp(other.as_str())
    }
}

impl PartialOrd<&str> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &&str) -> Option<Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<ArchivedLettreAddress> for &str {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        (*self).partial_cmp(other.as_str())
    }
}

impl PartialOrd<String> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &String) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<ArchivedLettreAddress> for String {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Box<str>> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &Box<str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedLettreAddress> for Box<str> {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl PartialOrd<Cow<'_, str>> for ArchivedLettreAddress {
    fn partial_cmp(&self, other: &Cow<'_, str>) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl PartialOrd<ArchivedLettreAddress> for Cow<'_, str> {
    fn partial_cmp(&self, other: &ArchivedLettreAddress) -> Option<Ordering> {
        self.as_ref().partial_cmp(other.as_str())
    }
}

impl Hash for ArchivedLettreAddress {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) mod lettre_address {
        use std::hint::black_box;

        use proptest::{prop_assert_eq, proptest, strategy::Strategy, string::string_regex};
        use rkyv::{Serialize, rancor};

        use super::*;

        const EMAIL_USER: &str = r#"(?:[a-z0-9!#$%&'*+\x2f=?^_`\x7b-\x7d~\x2d]+(?:\.[a-z0-9!#$%&'*+\x2f=?^_`\x7b-\x7d~\x2d]+)*|"(?:[\x01-\x08\x0b\x0c\x0e-\x1f\x21\x23-\x5b\x5d-\x7f]|\\[\x01-\x09\x0b\x0c\x0e-\x7f])*")"#;
        const EMAIL_DOMAIN: &str = r"(?:(?:[a-z0-9](?:[a-z0-9\x2d]*[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9\x2d]*[a-z0-9])?|\[(?:(?:(2(5[0-5]|[0-4][0-9])|1[0-9][0-9]|[1-9]?[0-9]))\.){3}(?:(2(5[0-5]|[0-4][0-9])|1[0-9][0-9]|[1-9]?[0-9])|[a-z0-9\x2d]*[a-z0-9]:(?:[\x01-\x08\x0b\x0c\x0e-\x1f\x21-\x3f\x41-\x5a\x53-\x7f]|\\[\x01-\x09\x0b\x0c\x0e-\x3f\x41-\x7f])+)\])";

        pub(crate) fn strategy() -> impl Strategy<Value = lettre::Address> {
            (
                string_regex(EMAIL_USER)
                    .expect("valid regex")
                    .prop_filter("invalid user", |user| {
                        EmailAddress::is_valid_local_part(user)
                    }),
                string_regex(EMAIL_DOMAIN)
                    .expect("valid regex")
                    .prop_filter("invalid domain", |domain| {
                        EmailAddress::is_valid_domain(domain)
                    }),
            )
                .prop_map(|(user, domain)| lettre::Address::new_dangerous(&user, &domain))
        }

        #[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq)]
        #[rkyv(derive(Debug))]
        struct Test {
            #[rkyv(with = LettreAddress)]
            email: lettre::Address,
        }

        impl Test {
            fn strategy() -> impl Strategy<Value = Self> {
                strategy().prop_map(|email| Self { email })
            }
        }

        proptest! {
            #[test]
            fn no_panic(bytes: Vec<u8>) {
                drop(black_box(rkyv::access::<ArchivedLettreAddress, rancor::Error>(&bytes)));
            }

            #[test]
            fn round_trip(test in Test::strategy()) {
                let bytes = rkyv::to_bytes::<rancor::Error>(&test)?;
                prop_assert_eq!(rkyv::from_bytes::<Test, rancor::Error>(&bytes)?, test);
            }
        }

        /// Check that invalid [`lettre::Address`]es can't be accessed.
        #[test]
        fn access_err() -> Result<(), rancor::Error> {
            let invalid_user = Test {
                email: lettre::Address::new_dangerous("", "example.com"),
            };
            let bytes = rkyv::to_bytes(&invalid_user)?;
            rkyv::access::<ArchivedTest, rancor::Error>(&bytes).expect_err("invalid user");

            let invalid_domain = Test {
                email: lettre::Address::new_dangerous("test", ""),
            };
            let bytes = rkyv::to_bytes(&invalid_domain)?;
            rkyv::access::<ArchivedTest, rancor::Error>(&bytes).expect_err("invalid domain");

            Ok(())
        }
    }
}
