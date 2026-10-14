//! API routes nested under `/api/auth`.

use axum::{http::StatusCode, response::IntoResponse};
use itertools::Itertools;
use novelnote_core::user::{
    PasswordScore, ScorePasswordParameters, WeakPasswordError, score_password,
};
use tracing::instrument;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::api::{
    ClientErrorResponse, InternalServerErrorResponse,
    body::{RequestBody, ResponseContentType},
};

/// Path the [`router()`] should be nested under.
pub(super) const PATH: &str = "/auth";

/// Tag all routes in this module use.
pub(super) const TAG: &str = "auth";

/// Router for `/api/auth`.
pub(super) fn router<S: Clone + Send + Sync + 'static>() -> OpenApiRouter<S> {
    OpenApiRouter::default().routes(routes!(score_password_strength))
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

    score_password(&password, &user_inputs)
        .map(|score| (StatusCode::OK, content_type.into_body(score)))
        .map_err(|error| (StatusCode::BAD_REQUEST, content_type.into_body(error)))
}
