//! API routes nested under `/api/auth`.

mod token;

use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use argon2::{Argon2, PasswordHasher, PasswordVerifier, password_hash};
use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderValue, StatusCode, header::USER_AGENT, request::Parts},
    middleware::Next,
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
use tower::{Layer, Service};
use tracing::{debug, error, info, instrument};
use utoipa::openapi::{Content, Object, Ref, RefOr, Type, response::Response as OpenApiResponse};
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
pub(super) const SESSION_COOKIE: &str = "__HOST-session_id";

/// Router for `/api/auth`.
pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::default()
        .routes(routes!(score_password_strength))
        .routes(routes!(signup))
        .routes(routes!(login))
}

/// [`tower::Layer`] for applying middleware to routes requiring authentication.
///
/// The middleware adds a [`UserId`] extension to the [`Request`] if it has a valid and unexpired
/// session cookie or bearer token. Otherwise the request is rejected with a `401 Unauthorized`
/// status.
pub(super) fn layer<S>(
    state: AppState,
) -> impl Layer<
    S,
    Service: Service<Request, Response = Response, Error = Infallible, Future: Send>
                 + Clone
                 + Send
                 + Sync,
> + Clone
where
    S: Service<Request, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Future: Send,
{
    axum::middleware::from_fn_with_state(state, middleware)
}

/// Extractor for getting the ID of the authenticated user.
///
/// Requires that the authentication [`layer()`] was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UserId(pub(super) Uuid);

impl<S> FromRequestParts<S> for UserId {
    type Rejection = StatusCode;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let response = parts.extensions.get().copied().ok_or_else(|| {
            error!(
                "attempted to get an authenticated user ID for a route not covered by the \
                 authentication middleware",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        });
        std::future::ready(response)
    }
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

/// Middleware for requiring user authentication.
///
/// Adds a [`UserId`] extension to the request before calling the next operation in the stack if
/// the session ID in the session cookie or bearer token corresponds to a unexpired session.
///
/// # Errors
///
/// Returns an error response if the cookie or token value is invalid, a session cookie or bearer
/// token wasn't provided, the session ID from the cookie and token do not match, the session is
/// expired, or a database error occurred.
#[expect(
    clippy::result_large_err,
    reason = "cookie jar required in all responses"
)]
#[tracing::instrument(level = "trace", skip_all)]
async fn middleware(
    State(AppState {
        auth_config: Config {
            session_length: expires_in,
            ..
        },
        database,
    }): State<AppState>,
    mut cookies: PrivateCookieJar,
    token: Option<Token>,
    ClientIp(ip_address): ClientIp,
    UserAgent(user_agent): UserAgent,
    mut request: Request,
    next: Next,
) -> Result<(PrivateCookieJar, Response), (PrivateCookieJar, AuthenticationError)> {
    // Parse session ID from session cookie.
    let session_cookie = if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        if let Ok(session_id) = cookie.value().parse() {
            Some((cookie, session_id))
        } else {
            return Err((cookies.remove(cookie), AuthenticationError::CookieParse));
        }
    } else {
        None
    };

    // If bearer token and session cookie are both set, make sure they match.
    let (session_id, session_cookie) = match (token, session_cookie) {
        (None, None) => return Err((cookies, AuthenticationError::Missing)),
        (None, Some((cookie, session_id))) => (session_id, Some(cookie)),
        (Some(Token::Session(session_id)), None) => (session_id, None),
        (Some(Token::Session(token_session_id)), Some((cookie, cookie_session_id))) => {
            if token_session_id == cookie_session_id {
                (token_session_id, Some(cookie))
            } else {
                return Err((cookies.remove(cookie), AuthenticationError::Mismatch));
            }
        }
    };

    // Get the user ID for the session, checking if the session has expired and update it if not.
    let session_update = sessions::Update {
        ip_address,
        user_agent,
        expires_in,
    };
    let user_id = match database
        .sessions()
        .user_id_and_update(session_id, session_update)
        .await
    {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            if let Some(cookie) = session_cookie {
                cookies = cookies.remove(cookie);
            }
            return Err((cookies, AuthenticationError::Expired));
        }
        Err(error) => return Err((cookies, error.into())),
    };

    // Add the authenticated user ID to the request extensions for use in routes.
    request.extensions_mut().insert(UserId(user_id));

    // Refresh cookie expiration.
    if session_cookie.is_some() {
        cookies = cookies.add(cookie(&session_id, expires_in));
    }

    Ok((cookies, next.run(request).await))
}

/// Error returned when the authentication [`middleware()`] fails.
///
/// Can also be used as a response in an [`OpenApi`](utoipa::openapi::OpenApi) document.
#[derive(Debug)]
pub(super) enum AuthenticationError {
    /// Error parsing cookie value as [`Uuid`].
    CookieParse,

    /// Request did not have a session cookie or bearer token.
    Missing,

    /// Session ID from the session cookie did not match the one from the bearer token.
    Mismatch,

    /// Session is expired.
    Expired,

    /// Database error while retrieving user ID from session or updating the session.
    Database(ExecuteError),
}

impl AuthenticationError {
    /// Name used when referencing this response in the OpenAPI document.
    const RESPONSE_NAME: &str = "AuthenticationError";
}

impl From<ExecuteError> for AuthenticationError {
    fn from(value: ExecuteError) -> Self {
        Self::Database(value)
    }
}

impl IntoResponse for AuthenticationError {
    fn into_response(self) -> Response {
        let message = match self {
            Self::CookieParse => "error parsing session ID from cookie",
            Self::Missing => "session cookie or bearer token is required for authentication",
            Self::Mismatch => "session ID from cookie did not match bearer token",
            Self::Expired => "session is expired",
            Self::Database(error) => {
                error!(
                    ?error,
                    "database error while getting session's user ID or updating it"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

        (StatusCode::UNAUTHORIZED, message).into_response()
    }
}

impl utoipa::IntoResponses for AuthenticationError {
    fn responses() -> BTreeMap<String, RefOr<OpenApiResponse>> {
        let status = StatusCode::UNAUTHORIZED;
        let response = Ref::from_response_name(Self::RESPONSE_NAME);

        [(status.as_str().to_owned(), response.into())].into()
    }
}

impl utoipa::ToResponse<'static> for AuthenticationError {
    fn response() -> (&'static str, RefOr<OpenApiResponse>) {
        let schema = Object::builder()
            .schema_type(Type::String)
            .description(Some("The body will contain a description of the error."));

        let response = OpenApiResponse::builder()
            .description("Authentication is missing, invalid, or session expired.")
            .content(
                mime::TEXT_PLAIN_UTF_8.as_ref(),
                Content::builder()
                    .schema(Some(schema))
                    .example(Some("session is expired".into())),
            )
            .build();

        (Self::RESPONSE_NAME, response.into())
    }
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
) -> Result<ResponseBody<PasswordScore>, (StatusCode, ResponseBody<WeakPasswordError>)> {
    let user_inputs = user_inputs.iter().map(String::as_str).collect_vec();

    score_password(password.reveal(), &user_inputs)
        .map(|score| content_type.into_body(score))
        .map_err(|error| (StatusCode::BAD_REQUEST, content_type.into_body(error)))
}

/// Signup
///
/// Create a new user and log in to the new account.
///
/// On success, sets an `HttpOnly` cookie that can be used for authentication. If the client does
/// not support cookies, a bearer token is included in the response body that can be used in a
/// [`Authorization`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Authorization)
/// header.
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
    ClientIp(ip_address): ClientIp,
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
///
/// On success, sets an `HttpOnly` cookie that can be used for authentication. If the client does
/// not support cookies, a bearer token is included in the response body that can be used in a
/// [`Authorization`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Authorization)
/// header.
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
    ClientIp(ip_address): ClientIp,
    UserAgent(user_agent): UserAgent,
    cookies: PrivateCookieJar,
    content_type: ResponseContentType,
    RequestBody(LoginCredentials { username, password }): RequestBody<LoginCredentials>,
) -> Result<(PrivateCookieJar, ResponseBody<Authenticated>), LoginError> {
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

    Ok((cookies, content_type.into_body(auth)))
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
    Cookie::build((SESSION_COOKIE, session_id.as_simple().to_string()))
        .http_only(true)
        .max_age(max_age.try_into().unwrap_or(SignedDuration::MAX))
        .secure(true)
        .path("/")
        .build()
}
