//! API routes nested under `/api`.

use axum::{Router, http::StatusCode, routing::get};

/// Router for `/api`.
pub(crate) fn router() -> Router {
    Router::new().route("/health-check", get(StatusCode::OK))
}
