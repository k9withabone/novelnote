//! `/api/auth` routes and responses.

use std::sync::Arc;

pub use novelnote_core::user::{PasswordScore, WeakPasswordError};
use novelnote_core::{
    Secret,
    user::{Authenticated, InvalidSignupCredentials, ScorePasswordParameters, SignupCredentials},
};
use reqwest::StatusCode;
use thiserror::Error;

use crate::{
    AuthenticatedClient, Client, DeserializeError, PostError, ResponseExt, UnexpectedError,
};

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
    /// the password was too weak, or the server returned an unexpected response.
    pub async fn score_password(
        &self,
        password: Secret,
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
            _ => Err(UnexpectedError::new(response).await.into()),
        }
    }

    /// Signup a new [`User`](novelnote_core::User) using password authentication.
    ///
    /// See [`score_password()`](Self::score_password()) for password requirements.
    ///
    /// Setting `cookie_authentication` to `true` causes the returned [`AuthenticatedClient`] to
    /// depend on an `HttpOnly` cookie set by the server for authentication. If the
    /// [`reqwest::Client`] this [`Client`] was created with does not have a cookie store enabled
    /// (or provided by a browser), then all calls with the [`AuthenticatedClient`] will fail.
    ///
    /// Sends a `POST` request to `/api/auth/signup`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent, the response body cannot be deserialized,
    /// the credentials were invalid (weak password, existing username or email), user signup with a
    /// password has been disabled, or the server returned an unexpected response.
    pub async fn signup(
        &self,
        credentials: &SignupCredentials,
        cookie_authentication: bool,
    ) -> Result<AuthenticatedClient, SignupError> {
        let response = self
            .client
            .post_body("api/auth/signup", credentials)
            .await?;

        match response.status() {
            StatusCode::CREATED if response.is_rkyv() => {
                let Authenticated { user, token } = response.rkyv().await?;
                Ok(AuthenticatedClient {
                    client: self.client.clone(),
                    token: (!cookie_authentication).then(|| Arc::from(token)),
                    user: Arc::new(user),
                })
            }
            StatusCode::BAD_REQUEST if response.is_rkyv() => {
                Err(response.rkyv::<InvalidSignupCredentials>().await?.into())
            }
            StatusCode::FORBIDDEN => Err(SignupError::Disabled),
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

    /// Unexpected response from the server.
    #[error("unexpected response from the server")]
    Unexpected(#[from] UnexpectedError),
}

/// Error returned from [`Auth::signup()`].
#[derive(Error, Debug)]
pub enum SignupError {
    /// Error sending request.
    #[error("error sending request")]
    Send(#[from] PostError),

    /// Error deserializing response body.
    #[error("error deserializing response body")]
    Deserialize(#[from] DeserializeError),

    /// Password was too weak.
    #[error("password was too weak")]
    InvalidCredentials(#[from] InvalidSignupCredentials),

    /// User signup with a password has been disabled.
    #[error("user signup with a password has been disabled")]
    Disabled,

    /// Unexpected response from the server.
    #[error("unexpected response from the server")]
    Unexpected(#[from] UnexpectedError),
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::{assert_matches, error::Error};

    use novelnote_database::Database;

    use crate::tests::with_client;

    use super::*;

    const STRONG_PASSWORD: &str = "octane slam pushover blah";

    mod score_password {
        use super::*;

        #[tokio::test]
        async fn ok() -> Result<(), Box<dyn Error>> {
            let score = with_client(
                "test_score_password_ok",
                Database::open_in_memory(1).await?,
                async |client| {
                    client
                        .auth()
                        .score_password(STRONG_PASSWORD.into(), Vec::new())
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
                "test_score_password_weak_password_err",
                Database::open_in_memory(1).await?,
                async |client| {
                    client
                        .auth()
                        .score_password("test".into(), Vec::new())
                        .await
                },
            )
            .await?;

            assert_matches!(
                result,
                Err(ScorePasswordError::WeakPassword(WeakPasswordError {
                    score: PasswordScore::Zero,
                    ..
                })),
            );

            Ok(())
        }
    }

    mod signup {
        use std::{collections::HashSet, path::PathBuf, time::Duration};

        use novelnote_core::{User, user::EmailAddress};
        use novelnote_database::users::{Authentication, NewUser};
        use novelnote_server::{BindAddress, SecretKey, Server};
        use tempfile::TempDir;
        use tokio::sync::oneshot;
        use tokio_util::sync::CancellationToken;
        use uuid::Uuid;

        use super::*;

        async fn database_with_user() -> Result<(Database, NewUser), Box<dyn Error>> {
            let database = Database::open_in_memory(1).await?;

            let new_user = NewUser {
                user: User {
                    id: Uuid::nil(),
                    username: "test".parse()?,
                    display_name: None,
                },
                email: Some(EmailAddress::Unverified("test@example.com".parse()?)),
                authentication: Authentication::PasswordHash("hash".to_owned()),
            };
            database.users().insert(new_user.clone()).await?;

            Ok((database, new_user))
        }

        #[tokio::test]
        async fn created() -> Result<(), Box<dyn Error>> {
            let credentials = SignupCredentials {
                username: "test".parse()?,
                display_name: Some("test".parse()?),
                email: None,
                password: STRONG_PASSWORD.into(),
            };

            let user = with_client(
                "test_signup_created",
                Database::open_in_memory(1).await?,
                async |client| client.auth().signup(&credentials, false).await,
            )
            .await??
            .user;

            assert_eq!(
                (&user.username, &user.display_name),
                (&credentials.username, &credentials.display_name),
            );

            Ok(())
        }

        #[tokio::test]
        async fn weak_password_err() -> Result<(), Box<dyn Error>> {
            let credentials = SignupCredentials {
                username: "test".parse()?,
                display_name: None,
                email: None,
                password: "test".into(),
            };

            let result = with_client(
                "test_signup_weak_password_err",
                Database::open_in_memory(1).await?,
                async |client| client.auth().signup(&credentials, false).await,
            )
            .await?;

            assert_matches!(
                result,
                Err(SignupError::InvalidCredentials(
                    InvalidSignupCredentials::WeakPassword(WeakPasswordError {
                        score: PasswordScore::Zero,
                        ..
                    }),
                )),
            );

            Ok(())
        }

        #[tokio::test]
        async fn existing_username_err() -> Result<(), Box<dyn Error>> {
            let (database, new_user) = database_with_user().await?;

            let credentials = SignupCredentials {
                username: new_user.user.username,
                display_name: None,
                email: None,
                password: STRONG_PASSWORD.into(),
            };

            let result = with_client(
                "test_signup_existing_username_err",
                database,
                async |client| client.auth().signup(&credentials, false).await,
            )
            .await?;

            assert_matches!(
                result,
                Err(SignupError::InvalidCredentials(
                    InvalidSignupCredentials::ExistingUsername,
                )),
            );

            Ok(())
        }

        #[tokio::test]
        async fn existing_email_err() -> Result<(), Box<dyn Error>> {
            let (database, new_user) = database_with_user().await?;

            let credentials = SignupCredentials {
                username: "test2".parse()?,
                display_name: None,
                email: new_user.email.map(Into::into),
                password: STRONG_PASSWORD.into(),
            };

            let result = with_client("test_signup_existing_email_err", database, async |client| {
                client.auth().signup(&credentials, false).await
            })
            .await?;

            assert_matches!(
                result,
                Err(SignupError::InvalidCredentials(
                    InvalidSignupCredentials::ExistingEmail,
                )),
            );

            Ok(())
        }

        #[tokio::test]
        async fn disabled_err() -> Result<(), Box<dyn Error>> {
            let temp_dir = TempDir::new()?;
            let path = temp_dir
                .path()
                .join("test_signup_disabled_err")
                .with_extension("socket");

            let server = Server {
                bind_address: BindAddress::Unix(path.clone()),
                trusted_proxies: HashSet::new(),
                asset_directory: PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../target/dist",
                )),
                password_signup_enabled: false,
                session_length: Duration::from_secs(60),
                secret_key: SecretKey::new([0; 64])?,
                database: Database::open_in_memory(1).await?,
            };

            let (ready_sender, ready_receiver) = oneshot::channel();
            let cancellation_token = CancellationToken::new();
            let server = tokio::spawn(server.run(
                ready_sender,
                cancellation_token.child_token().cancelled_owned(),
            ));

            let client = reqwest::Client::builder().unix_socket(path).build()?;
            let client = Client::new(client, "http://example.com".parse()?);

            let credentials = SignupCredentials {
                username: "test".parse()?,
                display_name: None,
                email: None,
                password: STRONG_PASSWORD.into(),
            };

            ready_receiver.await?;
            let result = client.auth().signup(&credentials, false).await;

            cancellation_token.cancel();
            server.await??;
            temp_dir.close()?;

            assert_matches!(result, Err(SignupError::Disabled));

            Ok(())
        }
    }
}
