//! Methods for interacting with the `users` table.

use std::fmt::{self, Display, Formatter};

use novelnote_core::{Name, User, Username, user::EmailAddress};
use tracing::instrument;
use uuid::Uuid;

use crate::{AsParams, Database, ExecuteError, RowExt, RusqliteConnectionExt};

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
    #[instrument(level = "trace", skip_all, fields(new_user.user.id))]
    pub async fn insert(&self, new_user: NewUser) -> Result<(), InsertError> {
        self.database
            .transaction(move |transaction| {
                if transaction.query_value(
                    "SELECT count(*) > 0 FROM users WHERE id = ?1",
                    new_user.user.id,
                )? {
                    Ok(Err(UniqueColumn::Id.into()))
                } else if transaction.query_value(
                    "SELECT count(*) > 0 FROM users WHERE username = ?1",
                    &new_user.user.username,
                )? {
                    Ok(Err(UniqueColumn::Username.into()))
                } else if let Authentication::Oidc(openid) = &new_user.authentication
                    && transaction
                        .query_value("SELECT count(*) > 0 FROM users WHERE openid = ?1", openid)?
                {
                    Ok(Err(UniqueColumn::OpenId.into()))
                } else if let Some(email) = &new_user.email
                    && transaction
                        .query_value("SELECT count(*) > 0 FROM users WHERE email = ?1", email)?
                {
                    Ok(Err(UniqueColumn::Email.into()))
                } else {
                    transaction.execute(
                        "INSERT INTO users (\
                             id, \
                             username, \
                             password_hash, \
                             openid, \
                             email, \
                             email_verified, \
                             display_name\
                         ) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        new_user.as_params(),
                    )?;

                    Ok(Ok(()))
                }
            })
            .await?
    }

    /// Get a [`User`] from the database using the user's ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn by_id(&self, id: Uuid) -> Result<Option<User>, ExecuteError> {
        self.database
            .query_optional_cached(
                "SELECT username, display_name FROM users WHERE id = ?1",
                id,
                move |row| {
                    Ok(User {
                        id,
                        username: row.parse_str(0)?,
                        display_name: row.parse_str_or_none(1)?,
                    })
                },
            )
            .await
    }

    /// Delete a user from the database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn delete(&self, id: Uuid) -> Result<(), ExecuteError> {
        self.database
            .execute("DELETE FROM users WHERE id = ?1", id)
            .await
    }
}

/// New user to [`insert`](Users::insert()) into the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    /// New user details.
    pub user: User,

    /// The user's email address.
    ///
    /// If enabled by the server admin, can be used for password resets and notifications.
    pub email: Option<EmailAddress>,

    /// Authentication option.
    pub authentication: Authentication,
}

impl AsParams for NewUser {
    fn as_params(&self) -> impl rusqlite::Params {
        let Self {
            user:
                User {
                    id,
                    username,
                    display_name,
                },
            email,
            authentication,
        } = self;

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

/// [`User`] authentication options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    /// Password hash for password login.
    PasswordHash(String),

    /// OpenID Connect subject identifier for login via OIDC.
    Oidc(String),
}

/// Error returned when [inserting](Users::insert()) a new user fails.
pub type InsertError = crate::InsertError<UniqueColumn>;

/// Columns in the `users` table that must have a unique value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueColumn {
    /// `id` column.
    Id,

    /// `username` column.
    Username,

    /// `openid` column.
    OpenId,

    /// `email` column.
    Email,
}

impl From<UniqueColumn> for InsertError {
    fn from(value: UniqueColumn) -> Self {
        Self::Duplicate(value)
    }
}

impl UniqueColumn {
    /// Unique `users` column as a static string slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Username => "username",
            Self::OpenId => "openid",
            Self::Email => "email",
        }
    }
}

impl Display for UniqueColumn {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsParams for Username {
    fn as_params(&self) -> impl rusqlite::Params {
        [self.as_str()]
    }
}

impl AsParams for EmailAddress {
    fn as_params(&self) -> impl rusqlite::Params {
        [self.as_str()]
    }
}

#[cfg(test)]
mod tests {
    use std::{assert_matches, error::Error};

    use super::*;

    async fn setup() -> Result<(Database, NewUser), Box<dyn Error>> {
        let database = Database::open_in_memory(1).await?;

        let id = Uuid::nil();
        let user = User {
            id,
            username: Username::new("test")?,
            display_name: Some(Name::new("test")?),
        };
        let new_user = NewUser {
            user,
            email: Some(EmailAddress::Unverified("test@example.com".parse()?)),
            authentication: Authentication::Oidc("openid".to_owned()),
        };

        database.users().insert(new_user.clone()).await?;

        Ok((database, new_user))
    }

    #[tokio::test]
    async fn duplicate_id_err() -> Result<(), Box<dyn Error>> {
        let (database, new_user) = setup().await?;

        let new_user = NewUser {
            user: User {
                id: new_user.user.id,
                username: Username::new("test2")?,
                display_name: None,
            },
            email: None,
            authentication: Authentication::PasswordHash("hash".to_owned()),
        };

        assert_matches!(
            database.users().insert(new_user).await,
            Err(InsertError::Duplicate(UniqueColumn::Id)),
        );

        Ok(())
    }

    #[tokio::test]
    async fn duplicate_username_err() -> Result<(), Box<dyn Error>> {
        let (database, new_user) = setup().await?;

        let new_user = NewUser {
            user: User {
                id: Uuid::from_u128(1),
                username: new_user.user.username,
                display_name: None,
            },
            email: None,
            authentication: Authentication::PasswordHash("hash".to_owned()),
        };

        assert_matches!(
            database.users().insert(new_user).await,
            Err(InsertError::Duplicate(UniqueColumn::Username)),
        );

        Ok(())
    }

    #[tokio::test]
    async fn duplicate_openid_err() -> Result<(), Box<dyn Error>> {
        let (database, new_user) = setup().await?;

        let new_user = NewUser {
            user: User {
                id: Uuid::from_u128(1),
                username: Username::new("test2")?,
                display_name: None,
            },
            email: new_user.email,
            authentication: Authentication::PasswordHash("hash".to_owned()),
        };

        assert_matches!(
            database.users().insert(new_user).await,
            Err(InsertError::Duplicate(UniqueColumn::Email)),
        );

        Ok(())
    }

    #[tokio::test]
    async fn duplicate_email_err() -> Result<(), Box<dyn Error>> {
        let (database, new_user) = setup().await?;

        let new_user = NewUser {
            user: User {
                id: Uuid::from_u128(1),
                username: Username::new("test2")?,
                display_name: None,
            },
            email: None,
            authentication: new_user.authentication,
        };

        assert_matches!(
            database.users().insert(new_user).await,
            Err(InsertError::Duplicate(UniqueColumn::OpenId)),
        );

        Ok(())
    }

    #[tokio::test]
    async fn by_id_and_delete() -> Result<(), Box<dyn Error>> {
        let (database, new_user) = setup().await?;
        let id = new_user.user.id;

        assert_eq!(database.users().by_id(id).await?, Some(new_user.user));

        database.users().delete(id).await?;
        assert_eq!(database.users().by_id(id).await?, None);

        Ok(())
    }
}
