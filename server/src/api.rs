//! API routes nested under `/api`.

mod auth;
mod body;
mod user;

use std::collections::BTreeMap;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use itertools::chain;
use novelnote_database::Database;
use tracing::{error, instrument};
use utoipa::{
    Modify, OpenApi,
    openapi::{
        Content, Object, OpenApi as OpenApiObject, PathItem, Ref, RefOr,
        Response as OpenApiResponse, SecurityRequirement, Type,
        path::Operation,
        security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme},
    },
};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_redoc::{Redoc, Servable};

use crate::AppState;

pub(crate) use self::auth::Config as AuthConfig;
use self::{auth::AuthenticationError, body::MediaType};

/// Path the [`router()`] should be nested under.
pub(crate) const PATH: &str = "/api";

/// Router for `/api`.
///
/// Provides all routes for NovelNote's HTTP API and documentation via Redoc.
pub(crate) fn router(state: AppState) -> Router<AppState> {
    let mut router = OpenApiRouter::default()
        .nest(user::PATH, user::router())
        .route_layer(auth::layer(state));

    add_security_requirements(router.get_openapi_mut());

    let (router, mut openapi) = router
        .nest(auth::PATH, auth::router())
        .routes(routes!(health_check))
        .split_for_parts();

    add_content_types(&mut openapi);

    let openapi = ApiDoc::openapi().nest(PATH, openapi);

    router
        .route("/openapi.json", get(Json(openapi.clone())))
        .merge(Redoc::with_url("/redoc", openapi).custom_html(include_str!("api/redoc.html")))
}

/// API documentation.
///
/// Pulls other info fields from package metadata.
///
/// Paths are determined via [`OpenApiRouter::routes()`].
#[derive(OpenApi)]
#[openapi(
    info(
        title = "NovelNote HTTP API",
        description = "NovelNote is a self-hosted book tracker.\n\n\
            The OpenAPI document is available at [`/api/openapi.json`](/api/openapi.json).\n\n\
            Both request and response bodies may be in the `application/json`, `application/rkyv`, \
            or `application/x-www-form-urlencoded` formats. For all requests that include a body, \
            the [`Content-Type`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Type) \
            header **must** be set. The response body content type is determined by the \
            [`Accept`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Accept) \
            header in the request. If the `Accept` header is not set, the `Content-Type` header is \
            used if set. Otherwise, the default response body content type is `application/json`. \
            Response bodies may also be plain text if there is an error.\n\n\
            Most routes require a form of authentication. The [login](#tag/auth/operation/login) \
            and [signup](#tag/auth/operation/signup) routes provide an `HttpOnly` cookie and \
            bearer token for use in a [`Authorization`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Authorization) \
            header. Either of these authentication methods may be used. If both are present, they \
            must correspond to the same session ID or the request will be rejected.",
    ),
    tags(
        (name = auth::TAG, description = "Authentication related routes."),
        (name = user::TAG, description = "Authenticated user routes."),
    ),
    components(responses(AuthenticationError, ClientErrorResponse, InternalServerErrorResponse)),
    modifiers(&SecuritySchemes),
)]
struct ApiDoc;

/// Add [`SecurityRequirement`]s to all [`Operation`]s.
fn add_security_requirements(openapi: &mut OpenApiObject) {
    for operation in operations(openapi) {
        operation.security.get_or_insert_default().extend(
            [SecuritySchemes::COOKIE, SecuritySchemes::BEARER_TOKEN]
                .into_iter()
                .map(|scheme| SecurityRequirement::new(scheme, None::<&str>)),
        );
    }
}

/// Add `application/rkyv` and `application/x-www-form-urlencoded` content types to the request
/// bodies and responses of all [`Operation`]s, cloning the `application/json` schema.
fn add_content_types(openapi: &mut OpenApiObject) {
    for operation in operations(openapi) {
        if let Some(RefOr::T(body)) = &mut operation.request_body
            && let Some(RefOr::T(Content { schema, .. })) = body.content.get(MediaType::JSON)
        {
            let content: RefOr<Content> = Content::builder().schema(schema.clone()).into();
            body.content.extend([
                (MediaType::RKYV.to_owned(), content.clone()),
                (MediaType::FORM.to_owned(), content),
            ]);
        }

        for response in operation.responses.responses.values_mut() {
            if let RefOr::T(response) = response
                && let Some(RefOr::T(Content { schema, .. })) =
                    response.content.get(MediaType::JSON)
            {
                let content: RefOr<Content> = Content::builder().schema(schema.clone()).into();
                response.content.extend([
                    (MediaType::RKYV.to_owned(), content.clone()),
                    (MediaType::FORM.to_owned(), content),
                ]);
            }
        }
    }
}

/// Iterator of all [`Operation`]s in the OpenAPI object.
fn operations(openapi: &mut OpenApiObject) -> impl Iterator<Item = &mut Operation> {
    openapi.paths.paths.values_mut().flat_map(
        |PathItem {
             get,
             put,
             post,
             delete,
             options,
             head,
             patch,
             trace,
             query,
             additional_operations,
             ..
         }| {
            chain!(
                get,
                put,
                post,
                delete,
                options,
                head,
                patch,
                trace,
                query,
                additional_operations.values_mut(),
            )
        },
    )
}

/// OpenAPI [`Response`](OpenApiResponse) indicating an error with the client's request.
///
/// Routes that use [`RequestBody`](body::RequestBody) should use this.
///
/// Can be used with `#[utoipa::path(responses(ClientErrorResponse))]`.
struct ClientErrorResponse;

impl ClientErrorResponse {
    /// Name used when referencing this response in the OpenAPI document.
    const RESPONSE_NAME: &str = "ClientError";
}

impl utoipa::IntoResponses for ClientErrorResponse {
    fn responses() -> BTreeMap<String, RefOr<OpenApiResponse>> {
        let response = Ref::from_response_name(Self::RESPONSE_NAME);

        [("4XX".to_owned(), response.into())].into()
    }
}

impl utoipa::ToResponse<'static> for ClientErrorResponse {
    fn response() -> (&'static str, RefOr<OpenApiResponse>) {
        let schema = Object::builder()
            .schema_type(Type::String)
            .description(Some(format!(
                "Possible 4XX status codes:\n\n\
                 - {}\n  \
                   - Error parsing `Accept` or `Content-Type` header value.\n  \
                   - Deserialization error.\n\
                 - {}\n  \
                   - Body was too large.\n\
                 - {}\n  \
                   - `Content-Type` header was not set or was an unsupported value.\n\
                 - {}\n  \
                   - Error deserializing body as desired schema.\n\n\
                 The body will contain a description of the error.",
                StatusCode::BAD_REQUEST,
                StatusCode::PAYLOAD_TOO_LARGE,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                StatusCode::UNPROCESSABLE_ENTITY,
            )));

        let response = OpenApiResponse::builder()
            .description("Client error.")
            .content(
                mime::TEXT_PLAIN_UTF_8.as_ref(),
                Content::builder().schema(Some(schema)).example(Some(
                    "error deserializing request body into target type".into(),
                )),
            )
            .build();

        (Self::RESPONSE_NAME, response.into())
    }
}

/// OpenAPI [`Response`](OpenApiResponse) indicating an internal server error.
///
/// Routes that use [`ResponseBody`](body::ResponseBody) should use this.
///
/// Can be used with `#[utoipa::path(responses(InternalServerErrorResponse))]`.
struct InternalServerErrorResponse;

impl InternalServerErrorResponse {
    /// Name used when referencing this response in the OpenAPI document.
    const RESPONSE_NAME: &str = "InternalServerError";
}

impl utoipa::IntoResponses for InternalServerErrorResponse {
    fn responses() -> BTreeMap<String, RefOr<OpenApiResponse>> {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        let response = Ref::from_response_name(Self::RESPONSE_NAME);

        [(status.as_str().to_owned(), response.into())].into()
    }
}

impl utoipa::ToResponse<'static> for InternalServerErrorResponse {
    fn response() -> (&'static str, RefOr<OpenApiResponse>) {
        let schema = Object::builder()
            .schema_type(Type::String)
            .description(Some(
                "The body may contain a description of the server error. If it does not, the \
                 `Content-Type` header will not be set.",
            ));

        let response = OpenApiResponse::builder()
            .description("Internal server error.")
            .content(
                mime::TEXT_PLAIN_UTF_8.as_ref(),
                Content::builder()
                    .schema(Some(schema))
                    .example(Some("error serializing response".into())),
            )
            .build();

        (Self::RESPONSE_NAME, response.into())
    }
}

/// OpenAPI [`SecurityScheme`]s to add to the [`ApiDoc`] components.
struct SecuritySchemes;

impl SecuritySchemes {
    /// Cookie security scheme name.
    const COOKIE: &str = "cookie";

    /// Bearer token security scheme name.
    const BEARER_TOKEN: &str = "bearer_token";
}

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut OpenApiObject) {
        openapi
            .components
            .get_or_insert_default()
            .add_security_schemes_from_iter([
                (
                    Self::COOKIE,
                    SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                        auth::SESSION_COOKIE,
                        "`HttpOnly` cookie obtained from the [login](#tag/auth/operation/login) \
                         or [signup](#tag/auth/operation/signup) routes.",
                    ))),
                ),
                (
                    Self::BEARER_TOKEN,
                    SecurityScheme::Http(
                        Http::builder()
                            .scheme(HttpAuthScheme::Bearer)
                            .bearer_format("Encrypted Session ID")
                            .description(Some(
                                "Bearer token obtained from the [login](#tag/auth/operation/login) \
                                 or [signup](#tag/auth/operation/signup) routes.",
                            ))
                            .build(),
                    ),
                ),
            ]);
    }
}

/// Health Check
///
/// Check if the server is healthy and responding.
#[utoipa::path(
    get,
    path = "/health-check",
    tag = "miscellaneous",
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
