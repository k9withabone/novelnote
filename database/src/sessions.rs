//! Methods for interacting with the `sessions` table.

use std::{
    fmt::{self, Display, Formatter},
    net::IpAddr,
    time::Duration,
};

use jiff::Timestamp;
use novelnote_core::{TimestampExt, user::Session};
use tracing::instrument;
use uuid::Uuid;

use crate::{AsParams, Database, ExecuteError, RowExt, RusqliteConnectionExt};

/// [`Database`] wrapper for interacting with the `sessions` table.
#[derive(Debug, Clone, Copy)]
pub struct Sessions<'a> {
    /// Database connection.
    pub(crate) database: &'a Database,
}

impl Sessions<'_> {
    /// Insert a new session into the database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn insert(&self, session: NewSession) -> Result<(), InsertError> {
        self.database
            .transaction(move |transaction| {
                if transaction.query_value(
                    "SELECT count(*) > 0 FROM sessions WHERE id = ?1",
                    session.id,
                )? {
                    Ok(Err(UniqueColumn::Id.into()))
                } else {
                    transaction.execute(
                        "INSERT INTO sessions \
                         (id, user_id, ip_address, user_agent, started, expires) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        session.as_params(),
                    )?;

                    Ok(Ok(()))
                }
            })
            .await?
    }

    /// Get the user ID belonging to an unexpired session and update the session.
    ///
    /// Returns [`None`] and does not update the session if it is expired or does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self, update))]
    pub async fn user_id_and_update(
        &self,
        id: Uuid,
        update: Update,
    ) -> Result<Option<Uuid>, ExecuteError> {
        self.database
            .transaction(move |transaction| {
                let user_id = transaction.query_optional_cached(
                    "SELECT user_id FROM valid_sessions WHERE id = ?1",
                    id,
                    |row| row.get(0),
                )?;

                if user_id.is_some() {
                    transaction.execute_cached(
                        "UPDATE sessions \
                         SET ip_address = ?1, user_agent = ?2, expires = ?3 \
                         WHERE id = ?4",
                        (update, id),
                    )?;
                }

                Ok(user_id)
            })
            .await
    }

    /// Get all the unexpired sessions belonging to a user.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn by_user_id(&self, user_id: Uuid) -> Result<Vec<Session>, ExecuteError> {
        self.database
            .query(
                "SELECT ip_address, user_agent, started, expires \
                 FROM valid_sessions WHERE user_id = ?1",
                user_id,
                |row| {
                    Ok(Session {
                        ip_address: row.parse_str_or_none(0)?,
                        user_agent: row.get(1)?,
                        started: row.get(2)?,
                        expires: row.get(3)?,
                    })
                },
            )
            .await
    }

    /// Delete a session from the database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn delete(&self, id: Uuid) -> Result<(), ExecuteError> {
        self.database
            .execute("DELETE FROM sessions WHERE id = ?1", id)
            .await
    }

    /// Delete all expired sessions from the database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is closed or SQLite returned an error.
    #[instrument(level = "trace", skip(self))]
    pub async fn delete_expired(&self) -> Result<(), ExecuteError> {
        self.database
            .execute("DELETE FROM sessions WHERE expires <= unixepoch('now')", ())
            .await
    }
}

/// A new session to insert into the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// Session ID.
    pub id: Uuid,

    /// ID of the [`User`](novelnote_core::User) the session is for.
    pub user_id: Uuid,

    /// IP address the session's request came from.
    pub ip_address: Option<IpAddr>,

    /// `User-Agent` header from the session's request.
    pub user_agent: Option<String>,

    /// How long the session should be valid for without another request before expiring.
    pub expires_in: Duration,
}

impl AsParams for NewSession {
    fn as_params(&self) -> impl rusqlite::Params {
        let Self {
            id,
            user_id,
            ip_address,
            user_agent,
            expires_in,
        } = self;

        let started = Timestamp::now();
        let expires = started
            .saturating_add(expires_in)
            .expect("non-Span duration");

        (
            *id,
            *user_id,
            ip_address.as_ref().map(ToString::to_string),
            user_agent.as_deref(),
            started.as_rounded_second(),
            expires.as_rounded_second(),
        )
    }
}

/// Error returned when [inserting](Sessions::insert()) a new session fails.
pub type InsertError = crate::InsertError<UniqueColumn>;

/// Columns in the `sessions` table that must have a unique value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueColumn {
    /// `id` column.
    Id,
}

impl From<UniqueColumn> for InsertError {
    fn from(value: UniqueColumn) -> Self {
        Self::Duplicate(value)
    }
}

impl UniqueColumn {
    /// Unique `sessions` column as a static string slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
        }
    }
}

impl Display for UniqueColumn {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameters from a new request to update a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// IP address the session's request came from.
    pub ip_address: Option<IpAddr>,

    /// `User-Agent` header of the session's request.
    pub user_agent: Option<String>,

    /// How long the session should be valid for without another request before expiring.
    pub expires_in: Duration,
}

impl From<NewSession> for Update {
    fn from(value: NewSession) -> Self {
        let NewSession {
            id: _,
            user_id: _,
            ip_address,
            user_agent,
            expires_in,
        } = value;

        Self {
            ip_address,
            user_agent,
            expires_in,
        }
    }
}

impl AsParams for (Update, Uuid) {
    fn as_params(&self) -> impl rusqlite::Params {
        let (update, id) = self;
        let Update {
            ip_address,
            user_agent,
            expires_in,
        } = update;

        let expires = Timestamp::now()
            .saturating_add(expires_in)
            .expect("non-Span duration");

        (
            ip_address.as_ref().map(ToString::to_string),
            user_agent.as_deref(),
            expires.as_rounded_second(),
            *id,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{assert_matches, error::Error, iter::repeat_n, net::Ipv4Addr};

    use jiff::{SpanRound, ToSpan, Unit};
    use novelnote_core::{User, Username, user::InvalidUsernameError};

    use crate::users::{Authentication, NewUser};

    use super::*;

    fn user(id: Uuid, username: &str) -> Result<NewUser, InvalidUsernameError> {
        Ok(NewUser {
            user: User {
                id,
                username: Username::new(username)?,
                display_name: None,
            },
            email: None,
            authentication: Authentication::PasswordHash("hash".to_owned()),
        })
    }

    async fn setup() -> Result<(Database, Uuid), Box<dyn Error>> {
        let database = Database::open_in_memory(1).await?;

        let user_id = Uuid::nil();
        database.users().insert(user(user_id, "test")?).await?;

        Ok((database, user_id))
    }

    #[tokio::test]
    async fn cascade() -> Result<(), Box<dyn Error>> {
        let (database, user_id) = setup().await?;

        let session = NewSession {
            id: Uuid::from_u128(1),
            user_id,
            ip_address: None,
            user_agent: None,
            expires_in: Duration::from_secs(10),
        };

        database.sessions().insert(session).await?;
        assert_eq!(database.sessions().by_user_id(user_id).await?.len(), 1);

        database.users().delete(user_id).await?;
        assert_eq!(database.sessions().by_user_id(user_id).await?, []);

        Ok(())
    }

    #[tokio::test]
    async fn duplicate_id_err() -> Result<(), Box<dyn Error>> {
        let (database, user_id) = setup().await?;

        let session = NewSession {
            id: Uuid::from_u128(1),
            user_id,
            ip_address: None,
            user_agent: None,
            expires_in: Duration::from_secs(10),
        };

        database.sessions().insert(session.clone()).await?;

        assert_matches!(
            database.sessions().insert(session).await,
            Err(InsertError::Duplicate(UniqueColumn::Id)),
        );

        Ok(())
    }

    #[tokio::test]
    async fn user_id_and_update_and_delete() -> Result<(), Box<dyn Error>> {
        let (database, user_id) = setup().await?;

        let session_id = Uuid::from_u128(1);
        let session = NewSession {
            id: session_id,
            user_id,
            ip_address: None,
            user_agent: None,
            expires_in: Duration::from_secs(10),
        };
        database.sessions().insert(session).await?;

        let ip_address = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let user_agent = Some("example/0.1.0".to_owned());
        let expires_in = 2_i64.hours();
        let update = Update {
            ip_address,
            user_agent: user_agent.clone(),
            expires_in: expires_in.try_into()?,
        };

        assert_eq!(
            database
                .sessions()
                .user_id_and_update(session_id, update.clone())
                .await?,
            Some(user_id),
        );

        let sessions = database
            .sessions()
            .by_user_id(user_id)
            .await?
            .into_iter()
            .map(
                |Session {
                     ip_address,
                     user_agent,
                     started,
                     expires,
                 }| {
                    let expires_in = started
                        .until(expires)
                        .expect("valid Span")
                        .round(SpanRound::new().largest(Unit::Hour).smallest(Unit::Hour))
                        .expect("clean rounding")
                        .fieldwise();
                    (ip_address, user_agent, expires_in)
                },
            );
        itertools::assert_equal(
            sessions,
            Some((ip_address, user_agent, expires_in.fieldwise())),
        );

        database.sessions().delete(session_id).await?;
        assert_eq!(
            database
                .sessions()
                .user_id_and_update(session_id, update)
                .await?,
            None,
        );

        Ok(())
    }

    #[tokio::test]
    async fn by_user_id() -> Result<(), Box<dyn Error>> {
        let (database, user_id) = setup().await?;

        let ip_address = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let user_agent = Some("example/0.1.0".to_owned());
        let expires_in = 2_i64.hours();

        let session_1 = NewSession {
            id: Uuid::from_u128(1),
            user_id,
            ip_address,
            user_agent: user_agent.clone(),
            expires_in: expires_in.try_into()?,
        };
        let session_2 = NewSession {
            id: Uuid::from_u128(2),
            ..session_1.clone()
        };
        let session_3 = NewSession {
            id: Uuid::from_u128(3),
            ..session_1.clone()
        };
        database.sessions().insert(session_1.clone()).await?;
        database.sessions().insert(session_2.clone()).await?;
        database.sessions().insert(session_3.clone()).await?;

        let user_2_id = Uuid::from_u128(4);
        database.users().insert(user(user_2_id, "test_2")?).await?;

        let other_session = NewSession {
            id: Uuid::from_u128(5),
            user_id: user_2_id,
            ..session_1.clone()
        };
        database.sessions().insert(other_session).await?;

        let sessions = database
            .sessions()
            .by_user_id(user_id)
            .await?
            .into_iter()
            .map(
                |Session {
                     ip_address,
                     user_agent,
                     started,
                     expires,
                 }| {
                    let expires_in = started
                        .until(expires)
                        .expect("valid Span")
                        .round(SpanRound::new().largest(Unit::Hour))
                        .expect("balancing")
                        .fieldwise();
                    (ip_address, user_agent, expires_in)
                },
            );
        itertools::assert_equal(
            sessions,
            repeat_n((ip_address, user_agent, expires_in.fieldwise()), 3),
        );

        Ok(())
    }

    #[tokio::test]
    async fn delete_expired() -> Result<(), Box<dyn Error>> {
        let (database, user_id) = setup().await?;

        // Create a session that started in the past and has expired.
        let started = Timestamp::now().saturating_sub(2_i64.hours())?;
        let expires = started.saturating_add(1_i64.hour())?;
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO sessions \
                     (id, user_id, ip_address, user_agent, started, expires) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    (
                        Uuid::from_u128(1),
                        user_id,
                        None::<String>,
                        None::<String>,
                        started.as_rounded_second(),
                        expires.as_rounded_second(),
                    ),
                )
            })
            .await??;

        database.sessions().delete_expired().await?;
        assert_eq!(database.sessions().by_user_id(user_id).await?, []);

        Ok(())
    }
}
