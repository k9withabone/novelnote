//! `/api/auth` routes and responses.

use novelnote_core::user::ScorePasswordParameters;
pub use novelnote_core::user::{PasswordScore, WeakPasswordError};
use reqwest::StatusCode;
use thiserror::Error;

use crate::{Client, DeserializeError, PostError, ResponseExt, UnexpectedError};

/// A [`Client`] wrapper for `/api/auth` routes.
#[derive(Debug, Clone, Copy)]
pub struct Auth<'a> {
    /// HTTP API client.
    pub(crate) client: &'a Client,
}

impl Auth<'_> {
    /// Score a password's strength.
    ///
    /// Passwords are scored on a 0-4 scale based on the number of guesses it would take an attacker
    /// to crack it. A [`Three`](PasswordScore::Three) is required for the password to be considered
    /// strong enough.
    ///
    /// Sends a `POST` request to `/api/auth/score-password`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent, the response body cannot be deserialized,
    /// the password was too weak, there was an internal server error, or the server returned an
    /// unexpected response.
    pub async fn score_password(
        &self,
        password: String,
        user_inputs: Vec<String>,
    ) -> Result<PasswordScore, ScorePasswordError> {
        let parameters = ScorePasswordParameters {
            password,
            user_inputs,
        };

        let response = self
            .client
            .post_body("api/auth/score-password", &parameters)
            .await?;

        match response.status() {
            StatusCode::OK if response.is_rkyv() => Ok(response.rkyv().await?),
            StatusCode::BAD_REQUEST if response.is_rkyv() => {
                Err(response.rkyv::<WeakPasswordError>().await?.into())
            }
            StatusCode::INTERNAL_SERVER_ERROR => Err(ScorePasswordError::InternalServerError),
            _ => Err(UnexpectedError::new(response).await.into()),
        }
    }
}

/// Error returned from [`Auth::score_password()`].
#[derive(Error, Debug)]
pub enum ScorePasswordError {
    /// Error sending request.
    #[error("error sending request")]
    Send(#[from] PostError),

    /// Error deserializing response body.
    #[error("error deserializing response body")]
    Deserialize(#[from] DeserializeError),

    /// Password was too weak.
    #[error("password was too weak")]
    WeakPassword(#[from] WeakPasswordError),

    /// Internal server error.
    #[error("internal server error")]
    InternalServerError,

    /// Unexpected response from the server.
    #[error("unexpected response from the server")]
    Unexpected(#[from] UnexpectedError),
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    mod score_password {
        use std::error::Error;

        use novelnote_database::Database;

        use crate::tests::with_client;

        use super::*;

        #[tokio::test]
        async fn ok() -> Result<(), Box<dyn Error>> {
            let score = with_client(
                "test_score_password_ok",
                Database::open_in_memory(1).await?,
                async |client| {
                    client
                        .auth()
                        .score_password("octane slam pushover blah".to_owned(), Vec::new())
                        .await
                },
            )
            .await??;

            assert_eq!(score, PasswordScore::Four);

            Ok(())
        }

        #[tokio::test]
        async fn weak_password_err() -> Result<(), Box<dyn Error>> {
            let result = with_client(
                "test_weak_password_err",
                Database::open_in_memory(1).await?,
                async |client| {
                    client
                        .auth()
                        .score_password("test".to_owned(), Vec::new())
                        .await
                },
            )
            .await?;

            assert!(
                matches!(
                    result,
                    Err(ScorePasswordError::WeakPassword(WeakPasswordError {
                        score: PasswordScore::Zero,
                        ..
                    })),
                ),
                "result was not weak password error, result: {result:?}",
            );

            Ok(())
        }
    }
}
