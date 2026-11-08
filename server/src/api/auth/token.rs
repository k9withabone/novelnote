//! [`Token`] and related types.

use std::sync::LazyLock;

use aes_gcm::{AeadCore, AeadInPlace, Aes256Gcm, KeyInit, KeySizeUser};
use base64::{Engine, engine::general_purpose::NO_PAD};
use thiserror::Error;
use typenum::Unsigned;
use uuid::Uuid;

use crate::SecretKey;

/// Base64 codec [`Engine`].
static BASE64: LazyLock<base64::engine::Simd> =
    LazyLock::new(|| base64::engine::Simd::standard(NO_PAD));

/// Key used for encrypting a [`Token`].
pub(super) type Key = [u8; <Aes256Gcm as KeySizeUser>::KeySize::USIZE];

impl AsRef<Key> for SecretKey {
    fn as_ref(&self) -> &Key {
        self.as_bytes()
            .first_chunk()
            .expect("SecretKey is at least 64 bytes long")
    }
}

/// Token used to authenticate a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Token {
    /// Session token.
    Session(Uuid),
}

impl Token {
    /// Number of bytes used for [`Aes256Gcm`]'s nonce.
    const NONCE_LEN: usize = <Aes256Gcm as AeadCore>::NonceSize::USIZE;

    /// Number of bytes used to discriminate between [`Token`] variants.
    const DISCRIMINATOR_LEN: usize = 1;

    /// Length of a [`Uuid`] in bytes.
    const UUID_LEN: usize = Uuid::nil().as_bytes().len();

    /// Length of the message that is encrypted.
    const MESSAGE_LEN: usize = Self::DISCRIMINATOR_LEN + Self::UUID_LEN;

    /// Number of bytes used for [`Aes256Gcm`]'s tag.
    const TAG_LEN: usize = <Aes256Gcm as AeadCore>::TagSize::USIZE;

    /// Total number of bytes used for an encrypted [`Token`].
    const ENCRYPTED_LEN: usize = Self::NONCE_LEN + Self::MESSAGE_LEN + Self::TAG_LEN;

    /// Returns the variant discriminator.
    const fn discriminator(&self) -> u8 {
        match self {
            Self::Session(_) => 0,
        }
    }

    /// Returns the bytes that make up the inner [`Uuid`].
    const fn inner_bytes(&self) -> &uuid::Bytes {
        let Self::Session(uuid) = self;
        uuid.as_bytes()
    }

    /// Encrypt the token with AES-GCM and encode it with Base64.
    pub(super) fn encrypt(&self, key: &Key) -> String {
        let mut buf = [0; Self::ENCRYPTED_LEN];

        // Generate nonce.
        let (nonce, rest) = buf.split_first_chunk_mut().expect("array split");
        rand::fill(nonce);

        // Set the plaintext.
        let (message, tag_buf): (&mut [u8; Self::MESSAGE_LEN], _) =
            rest.split_first_chunk_mut().expect("array split");
        let [discriminator, uuid @ ..] = message;
        *discriminator = self.discriminator();
        *uuid = *self.inner_bytes();

        // Encrypt the message and set the tag.
        let tag = Aes256Gcm::new(key.into())
            .encrypt_in_place_detached((&*nonce).into(), &[], message)
            .expect("buffers are appropriately sized");
        tag_buf.copy_from_slice(&tag);

        // Base64 encode encrypted token.
        BASE64.encode(buf)
    }

    /// Decrypt a Base64 encoded token with AES-GCM.
    ///
    /// # Errors
    ///
    /// Returns an error if the token cannot be decoded or decrypted, is not the correct length,
    /// or it is of an unknown variant.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "will be used for authentication, needed now for testing"
        )
    )]
    pub(super) fn decrypt(input: &str, key: &Key) -> Result<Self, TokenDecryptionError> {
        let mut buf = [0; Self::ENCRYPTED_LEN];

        // Base64 decode encrypted token.
        if BASE64.decode_slice(input, &mut buf)? != buf.len() {
            return Err(TokenDecryptionError::Length);
        }

        // Decrypt the message.
        let (nonce, rest) = buf.split_first_chunk_mut().expect("array split");
        let (message, tag) = rest.split_last_chunk_mut().expect("array split");
        Aes256Gcm::new(key.into()).decrypt_in_place_detached(
            (&*nonce).into(),
            &[],
            message,
            (&*tag).into(),
        )?;

        let (discriminator, uuid) = message.split_first().expect("array split");
        match *discriminator {
            0 => Ok(Self::Session(Uuid::from_slice(uuid)?)),
            unknown => Err(TokenDecryptionError::UnknownVariant(unknown)),
        }
    }
}

/// Error returned when [`Token::decrypt()`] fails.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "will be used for authentication, needed now for testing"
    )
)]
pub(super) enum TokenDecryptionError {
    /// Error decoding token as Base64.
    #[error("error decoding token as base64")]
    Base64(#[source] base64::DecodeError),

    /// Error decrypting token from decoded bytes.
    #[error("invalid token value")]
    Decrypt(#[from] aes_gcm::Error),

    /// Token was not the correct length.
    #[error("token was not the correct length")]
    Length,

    /// Unknown token variant.
    #[error("unknown variant discriminator `{0}`")]
    UnknownVariant(u8),
}

impl From<base64::DecodeSliceError> for TokenDecryptionError {
    fn from(value: base64::DecodeSliceError) -> Self {
        match value {
            base64::DecodeSliceError::DecodeError(decode_error) => Self::Base64(decode_error),
            base64::DecodeSliceError::OutputSliceTooSmall => Self::Length,
        }
    }
}

impl From<uuid::Error> for TokenDecryptionError {
    fn from(_: uuid::Error) -> Self {
        Self::Length
    }
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use proptest::{prop_assert_eq, proptest};

    use super::*;

    proptest! {
        #[test]
        fn round_trip(uuid: u128, key: Key) {
            let token = Token::Session(Uuid::from_u128(uuid));

            prop_assert_eq!(Token::decrypt(&token.encrypt(&key), &key)?, token);
        }

        #[test]
        fn decrypt_no_panic(input: [u8; Token::ENCRYPTED_LEN], key: Key) {
            drop(black_box(Token::decrypt(&BASE64.encode(input), &key)));
        }
    }
}
