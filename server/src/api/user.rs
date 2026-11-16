//! API routes nested under `/api/user`.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use novelnote_core::User;
use novelnote_database::{Database, ExecuteError};
use thiserror::Error;
use tracing::{error, instrument};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    AppState,
    api::{
        AuthenticationError, InternalServerErrorResponse,
        auth::UserId,
        body::{ResponseBody, ResponseContentType},
    },
};

/// Path the [`router()`] should be nested under.
pub(super) const PATH: &str = "/user";

/// Tag all routes in this module use.
pub(super) const TAG: &str = "user";

/// Router for `/api/user`.
pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::default().routes(routes!(get))
}

/// Get User
///
/// Get the authenticated user.
#[utoipa::path(
    get,
    path = "/",
    tag = TAG,
    responses(
        (status = OK, description = "Authenticated user.", body = User),
        AuthenticationError,
        InternalServerErrorResponse,
    ),
)]
#[instrument(level = "debug", skip(database, content_type))]
async fn get(
    State(database): State<Database>,
    UserId(user_id): UserId,
    content_type: ResponseContentType,
) -> Result<ResponseBody<User>, GetError> {
    let user = database
        .users()
        .by_id(user_id)
        .await?
        .ok_or(GetError::Missing)?;

    Ok(content_type.into_body(user))
}

/// Error returned when [`get()`]ting the authenticated [`User`] fails.
#[derive(Error, Debug)]
enum GetError {
    /// Database error while getting user.
    #[error("error getting authenticated user from database")]
    Database(#[from] ExecuteError),

    /// User missing from database.
    #[error("authenticated user missing from database")]
    Missing,
}

impl IntoResponse for GetError {
    fn into_response(self) -> Response {
        match &self {
            Self::Database(error) => error!(?error, "{self}"),
            Self::Missing => error!("{self}"),
        }

        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
