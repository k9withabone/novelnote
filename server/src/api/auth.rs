//! API routes nested under `/api/auth`.

mod token;

use std::{convert::Infallible, time::Duration};

use argon2::{Argon2, PasswordHasher, PasswordVerifier, password_hash};
use axum::{
    Extension,
    extract::{FromRequestParts, State},
    http::{HeaderValue, StatusCode, header::USER_AGENT, request::Parts},
    response::{IntoResponse, Response},
};
use axum_extra::extract::{PrivateCookieJar, cookie::Cookie};
use itertools::{Itertools, chain};
use novelnote_core::{
    Name, Secret, User,
    user::{
        Authenticated, EmailAddress, InvalidSignupCredentials, LoginCredentials, PasswordScore,
        ScorePasswordParameters, SignupCredentials, WeakPasswordError, score_password,
    },
};
use novelnote_database::{
    ExecuteError,
    sessions::{self, NewSession},
    users::{self, Authentication, NewUser},
};
use thiserror::Error;
use time::SignedDuration;
use tokio::task::spawn_blocking;
use tracing::{debug, error, info, instrument};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    AppState, ClientIp, SecretKey,
    api::{
        ClientErrorResponse, InternalServerErrorResponse,
        body::{RequestBody, ResponseBody, ResponseContentType},
    },
};

use self::token::Token;

/// Path the [`router()`] should be nested under.
pub(super) const PATH: &str = "/auth";

/// Tag all routes in this module use.
pub(super) const TAG: &str = "auth";

/// Name of the cookie that a session's ID is stored in.
const COOKIE_NAME: &str = "__HOST-session_id";

/// Router for `/api/auth`.
pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::default()
        .routes(routes!(score_password_strength))
        .routes(routes!(signup))
        .routes(routes!(login))
}

/// Configuration required for `/api/auth` routes.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Whether signups using a password should be enabled.
    pub(crate) password_signup_enabled: bool,

    /// Secret key used for securing session cookies, tokens, and as a password pepper.
    pub(crate) secret_key: SecretKey,

    /// How long between requests a session should be valid for before expiring.
    pub(crate) session_length: Duration,
}

/// Score a Password's Strength
///
/// Passwords are scored on a 0-4 scale based on the number of guesses it would take an attacker to
/// crack it. A 3 is required for the password to be considered strong enough.
#[utoipa::path(
    post,
    path = "/score-password",
    tag = TAG,
    request_body = ScorePasswordParameters,
    responses(
        (
            status = OK,
            description = "Password strength is acceptable.",
            body = PasswordScore,
            example = 3,
        ),
        (
            status = BAD_REQUEST,
            description = "Password was too weak.",
            body = WeakPasswordError,
        ),
        ClientErrorResponse,
        InternalServerErrorResponse,
    ),
)]
#[instrument(level = "debug", skip_all)]
async fn score_password_strength(
    content_type: ResponseContentType,
    RequestBody(ScorePasswordParameters {
        password,
        user_inputs,
    }): RequestBody<ScorePasswordParameters>,
) -> impl IntoResponse {
    let user_inputs = user_inputs.iter().map(String::as_str).collect_vec();

    score_password(password.reveal(), &user_inputs)
        .map(|score| (StatusCode::OK, content_type.into_body(score)))
        .map_err(|error| (StatusCode::BAD_REQUEST, content_type.into_body(error)))
}

/// Signup
///
/// Create a new user and log in to the new account.
#[utoipa::path(
    post,
    path = "/signup",
    tag = TAG,
    request_body = SignupCredentials,
    responses(
        (status = CREATED, description = "New user was created.", body = Authenticated),
        (
            status = BAD_REQUEST,
            description = "Invalid credentials.",
            body = InvalidSignupCredentials,
        ),
        (status = FORBIDDEN, description = "User signup with a password is disabled."),
        ClientErrorResponse,
        InternalServerErrorResponse,
    ),
)]
#[instrument(level = "debug", skip_all)]
async fn signup(
    State(AppState {
        auth_config:
            Config {
                password_signup_enabled,
                secret_key,
                session_length: expires_in,
            },
        database,
    }): State<AppState>,
    Extension(ClientIp(ip_address)): Extension<ClientIp>,
    UserAgent(user_agent): UserAgent,
    cookies: PrivateCookieJar,
    content_type: ResponseContentType,
    RequestBody(SignupCredentials {
        username,
        email,
        display_name,
        password,
    }): RequestBody<SignupCredentials>,
) -> Result<(StatusCode, PrivateCookieJar, ResponseBody<Authenticated>), SignupError> {
    if !password_signup_enabled {
        return Err(SignupError::Disabled);
    }

    let user_inputs = chain!(
        Some(username.as_str()),
        email.as_ref().map(lettre::Address::as_ref),
        display_name.as_ref().map(Name::as_str),
    )
    .collect_vec();
    score_password(password.reveal(), &user_inputs)
        .map_err(|error| content_type.into_body(error))?;

    let password_hash = hash_password(password, secret_key.clone()).await?;

    let user = User {
        id: Uuid::now_v7(),
        username,
        display_name,
    };
    let new_user = NewUser {
        user: user.clone(),
        email: email.map(EmailAddress::Unverified),
        authentication: Authentication::PasswordHash(password_hash),
    };

    database
        .users()
        .insert(new_user)
        .await
        .map_err(|error| content_type.into_body(error))?;
    info!(%user.id, "new user");

    // TODO: send email verification

    let session_id = Uuid::now_v7();
    let session = NewSession {
        id: session_id,
        user_id: user.id,
        ip_address,
        user_agent,
        expires_in,
    };

    database.sessions().insert(session).await?;
    debug!(session.id = %session_id, "new session");

    let cookies = cookies.add(cookie(&session_id, expires_in));

    let auth = Authenticated {
        user,
        token: Token::Session(session_id).encrypt(secret_key.as_ref()),
    };

    Ok((StatusCode::CREATED, cookies, content_type.into_body(auth)))
}

/// Error returned when new user [`signup()`] fails.
#[derive(Error, Debug)]
enum SignupError {
    /// Server admin has disabled password signups.
    #[error("signup with a password is disabled")]
    Disabled,

    /// Password was too weak.
    #[error("password was too weak")]
    WeakPassword(#[from] ResponseBody<WeakPasswordError>),

    /// Error hashing password.
    #[error("error hashing password")]
    HashPassword(#[from] password_hash::Error),

    /// Error inserting new user into the database.
    #[error("error inserting new user into the database")]
    UserInsert(#[from] ResponseBody<users::InsertError>),

    /// Error inserting new session into the database.
    #[error("error inserting new session into the database")]
    SessionInsert(#[from] sessions::InsertError),
}

impl IntoResponse for SignupError {
    fn into_response(self) -> Response {
        match self {
            Self::Disabled => StatusCode::FORBIDDEN.into_response(),

            Self::WeakPassword(body) => (
                StatusCode::BAD_REQUEST,
                body.map_into::<InvalidSignupCredentials>(),
            )
                .into_response(),

            Self::HashPassword(error) => {
                error!(?error, message = %error, "error hashing password for new user");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }

            Self::UserInsert(ResponseBody(
                content_type,
                users::InsertError::Duplicate(users::UniqueColumn::Username),
            )) => (
                StatusCode::BAD_REQUEST,
                content_type.into_body(InvalidSignupCredentials::ExistingUsername),
            )
                .into_response(),

            Self::UserInsert(ResponseBody(
                content_type,
                users::InsertError::Duplicate(users::UniqueColumn::Email),
            )) => (
                StatusCode::BAD_REQUEST,
                content_type.into_body(InvalidSignupCredentials::ExistingEmail),
            )
                .into_response(),

            Self::UserInsert(ResponseBody(
                _,
                error @ (users::InsertError::Duplicate(
                    users::UniqueColumn::Id | users::UniqueColumn::OpenId,
                )
                | users::InsertError::Execute(_)),
            )) => {
                error!(?error, "error inserting new user into database");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }

            Self::SessionInsert(error) => {
                error!(?error, "error creating session for new user");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "new user created, error creating session",
                )
                    .into_response()
            }
        }
    }
}

/// Login
///
/// Log in to an existing user account using a password.
#[utoipa::path(
    post,
    path = "/login",
    tag = TAG,
    request_body = LoginCredentials,
    responses(
        (status = OK, description = "Log in successful.", body = Authenticated),
        (status = UNAUTHORIZED, description = "Invalid credentials."),
        ClientErrorResponse,
        InternalServerErrorResponse,
    ),
)]
#[tracing::instrument(level = "debug", skip_all)]
async fn login(
    State(AppState {
        auth_config:
            Config {
                password_signup_enabled: _,
                secret_key,
                session_length: expires_in,
            },
        database,
    }): State<AppState>,
    Extension(ClientIp(ip_address)): Extension<ClientIp>,
    UserAgent(user_agent): UserAgent,
    cookies: PrivateCookieJar,
    content_type: ResponseContentType,
    RequestBody(LoginCredentials { username, password }): RequestBody<LoginCredentials>,
) -> Result<(StatusCode, PrivateCookieJar, ResponseBody<Authenticated>), LoginError> {
    let Some((user, password_hash)) = database
        .users()
        .user_and_password_hash_by_username(username)
        .await?
    else {
        // Check against a default password hash to mitigate timing attacks.
        verify_password(
            password,
            "$argon2id\
             $v=19\
             $m=19456,t=2,p=1\
             $ozpbTdQG5lDNvCZxo7/BDQ$pB6nPG31rYu15XKnE+EvCbs+5Yc8VKRBn1VtpwZK8Jg"
                .to_owned(),
            secret_key,
        )
        .await?;

        return Err(LoginError::InvalidCredentials);
    };

    if !verify_password(password, password_hash, secret_key.clone()).await? {
        return Err(LoginError::InvalidCredentials);
    }

    let session_id = Uuid::now_v7();
    let session = NewSession {
        id: session_id,
        user_id: user.id,
        ip_address,
        user_agent,
        expires_in,
    };

    database.sessions().insert(session).await?;
    debug!(session.id = %session_id, "new session");

    let cookies = cookies.add(cookie(&session_id, expires_in));

    let auth = Authenticated {
        user,
        token: Token::Session(session_id).encrypt(secret_key.as_ref()),
    };

    Ok((StatusCode::OK, cookies, content_type.into_body(auth)))
}

/// Error returned when [`login()`] fails.
#[derive(Error, Debug)]
enum LoginError {
    /// Error getting [`User`] and password hash from the database.
    #[error("error getting user and password hash from the database")]
    RetrievalError(#[from] ExecuteError),

    /// Invalid credentials.
    #[error("invalid credentials")]
    InvalidCredentials,

    /// Error hashing password.
    #[error("error hashing password")]
    HashPassword(#[from] password_hash::Error),

    /// Error inserting new session into the database.
    #[error("error inserting new session into the database")]
    SessionInsert(#[from] sessions::InsertError),
}

impl IntoResponse for LoginError {
    fn into_response(self) -> Response {
        match &self {
            Self::RetrievalError(error) => {
                error!(?error, "{self}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::HashPassword(error) => {
                error!(?error, message = %error, "{self} for login");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::SessionInsert(error) => {
                error!(?error, "{self}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
        .into_response()
    }
}

/// Extractor for optionally getting the `User-Agent` header from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserAgent(Option<String>);

impl<S> FromRequestParts<S> for UserAgent {
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let user_agent = parts
            .headers
            .get(USER_AGENT)
            .map(HeaderValue::to_str)
            .and_then(Result::ok)
            .map(ToOwned::to_owned);

        std::future::ready(Ok(Self(user_agent)))
    }
}

/// Compute the hash for a password.
///
/// # Errors
///
/// Returns an error if the password is longer than 4 GiB or enough random numbers could not be
/// provided by the system for the salt.
async fn hash_password(
    password: Secret,
    pepper: SecretKey,
) -> Result<String, password_hash::Error> {
    spawn_blocking(move || {
        argon2(&pepper)
            .hash_password(password.reveal_bytes())
            .map(|hash| hash.to_string())
    })
    .await
    .expect("`argon2` panicked")
}

/// Check if a password matches the expected hash.
///
/// Returns `Ok(true)` if it matches and `Ok(false)` if the password in invalid.
///
/// # Errors
///
/// Returns an error if the password hash cannot be parsed as a
/// [`PasswordHash`](password_hash::phc::PasswordHash) or the password is longer than 4 GiB.
async fn verify_password(
    password: Secret,
    password_hash: String,
    pepper: SecretKey,
) -> Result<bool, password_hash::Error> {
    match spawn_blocking(move || {
        argon2(&pepper).verify_password(password.reveal_bytes(), password_hash.as_str())
    })
    .await
    .expect("`argon2` panicked")
    {
        Ok(()) => Ok(true),
        Err(password_hash::Error::PasswordInvalid) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Create a new [`Argon2`] instance using a pepper and the default parameters.
fn argon2(pepper: &SecretKey) -> Argon2<'_> {
    use argon2::{Algorithm, Params, Version};

    Argon2::new_with_secret(
        pepper.as_bytes(),
        Algorithm::default(),
        Version::default(),
        Params::default(),
    )
    .expect("SecretKey is less than 4 GiB")
}

/// Construct a [`Cookie`] to store a session ID.
fn cookie(session_id: &Uuid, max_age: Duration) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, session_id.as_simple().to_string()))
        .http_only(true)
        .max_age(max_age.try_into().unwrap_or(SignedDuration::MAX))
        .secure(true)
        .path("/")
        .build()
}
