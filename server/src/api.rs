//! API routes nested under `/api`.

#![expect(clippy::needless_for_each, reason = "`OpenApi` derive")]

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use novelnote_database::Database;
use tracing::{error, instrument};
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_redoc::{Redoc, Servable};

/// Path the [`router()`] should be nested under.
pub(crate) const PATH: &str = "/api";

/// Router for `/api`.
///
/// Provides all routes for NovelNote's HTTP API and documentation via Redoc.
pub(crate) fn router() -> Router<Database> {
    let (router, openapi) = OpenApiRouter::default()
        .routes(routes!(health_check))
        .split_for_parts();

    let openapi = ApiDoc::openapi().nest(PATH, openapi);

    router
        .route("/openapi.json", get(Json(openapi.clone())))
        .merge(Redoc::with_url("/redoc", openapi))
}

/// API documentation.
///
/// Pulls other info fields from package metadata.
///
/// Paths are determined via [`OpenApiRouter::routes()`].
#[derive(OpenApi)]
#[openapi(info(
    title = "NovelNote HTTP API",
    description = "NovelNote is a self-hosted book tracker.

The OpenAPI document is available at [`/api/openapi.json`](/api/openapi.json).",
))]
struct ApiDoc;

/// Health Check
///
/// Check if the server is healthy and responding.
#[utoipa::path(
    get,
    path = "/health-check",
    responses(
        (status = OK, description = "Server is healthy."),
        (
            status = INTERNAL_SERVER_ERROR,
            description = "A server component is having issues, server restart recommended.",
        ),
    ),
)]
#[instrument(level = "debug", skip(database))]
async fn health_check(State(database): State<Database>) -> StatusCode {
    if database.is_open() {
        StatusCode::OK
    } else {
        error!("database connection is closed");
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
