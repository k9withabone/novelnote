//! Methods for interacting with the `users` table.

use novelnote_core::{Name, User, user::EmailAddress};
use tracing::instrument;
use uuid::Uuid;

use crate::{AsParams, Database, ExecuteError, RowExt};

/// [`Database`] wrapper for interacting with the `users` table.
#[derive(Debug, Clone, Copy)]
pub struct Users<'a> {
    /// Database connection.
    pub(crate) database: &'a Database,
}

impl Users<'_> {
    /// Insert a new [`User`] into the database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip_all, fields(user.id))]
    pub async fn insert(
        &self,
        user: User,
        authentication: Authentication,
    ) -> Result<(), ExecuteError> {
        self.database
            .execute(
                "INSERT INTO users \
                    (id, username, password_hash, openid, email, email_verified, display_name) \
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (user, authentication),
            )
            .await
    }

    /// Get a [`User`] from the database using the user's ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<User>, ExecuteError> {
        self.database
            .query_one_cached(
                "SELECT username, email, email_verified, display_name FROM users WHERE id = ?1",
                id,
                move |row| {
                    let username = row.parse_str(0)?;

                    let email = row.parse_str_or_none(1)?;
                    let email_verified = row.get(2)?;
                    let email = email.map(|email| {
                        if email_verified {
                            EmailAddress::Verified(email)
                        } else {
                            EmailAddress::Unverified(email)
                        }
                    });

                    let display_name = row.parse_str_or_none(3)?;

                    Ok(User {
                        id,
                        username,
                        email,
                        display_name,
                    })
                },
            )
            .await
    }
}

/// [`User`] authentication options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    /// Password hash for password login.
    PasswordHash(String),

    /// OpenID Connect subject identifier for login via OIDC.
    Oidc(String),
}

impl AsParams for (User, Authentication) {
    fn as_params(&self) -> impl rusqlite::Params {
        let (
            User {
                id,
                username,
                email,
                display_name,
            },
            authentication,
        ) = self;

        let (password_hash, openid) = match authentication {
            Authentication::PasswordHash(password_hash) => (Some(password_hash), None),
            Authentication::Oidc(openid) => (None, Some(openid)),
        };

        let (email, email_verified) = email
            .as_ref()
            .map_or_default(|email| (Some(email.as_str()), email.is_verified()));

        (
            *id,
            username.as_str(),
            password_hash,
            openid,
            email,
            email_verified,
            display_name.as_ref().map(Name::as_str),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use novelnote_core::Username;

    use super::*;

    #[tokio::test]
    async fn round_trip() -> Result<(), Box<dyn Error>> {
        let database = Database::open_in_memory(1).await?;

        let user = User {
            id: Uuid::nil(),
            username: Username::new("test")?,
            email: Some(EmailAddress::Verified("test@example.com".parse()?)),
            display_name: Some(Name::new("test")?),
        };
        let authentication = Authentication::PasswordHash("hash".to_owned());

        database
            .users()
            .insert(user.clone(), authentication)
            .await?;
        assert_eq!(database.users().get_by_id(user.id).await?, Some(user));

        Ok(())
    }
}
