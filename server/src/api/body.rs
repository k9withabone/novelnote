//! [`RequestBody`] and [`ResponseBody`].

use std::{future::Ready, num::ParseIntError, str::FromStr};

use axum::{
    Form, Json, RequestExt,
    extract::{
        FromRequest, FromRequestParts, Request,
        rejection::{FormRejection, JsonRejection},
    },
    http::{
        HeaderName, StatusCode,
        header::{self, ACCEPT, CONTENT_TYPE},
        request::Parts,
    },
    response::{IntoResponse, Response},
};
use http_body_util::LengthLimitError;
use itertools::{FoldWhile, Itertools};
use mime::Mime;
use rkyv::{
    Archive,
    api::high::{self, HighSerializer, HighValidator},
    bytecheck::CheckBytes,
    de::Pool,
    rancor::{self, Strategy},
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio_stream::{Stream, StreamExt};
use tracing::error;

/// Extractor for deserializing a request's body from a supported [`MediaType`].
pub(super) struct RequestBody<T>(pub(super) T);

impl<T, S> FromRequest<S> for RequestBody<T>
where
    T: DeserializeOwned + Archive,
    T::Archived: for<'a> CheckBytes<HighValidator<'a, rancor::BoxedError>>
        + rkyv::Deserialize<T, Strategy<Pool, rancor::BoxedError>>,
    S: Send + Sync,
{
    type Rejection = RequestBodyRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .ok_or(RequestBodyRejection::MissingContentType)?
            .to_str()?;
        let content_type =
            content_type
                .parse()
                .map_err(|source| RequestBodyRejection::InvalidMediaType {
                    value: content_type.to_owned(),
                    source,
                })?;
        let content_type = MediaType::try_from_mime(&content_type)
            .ok_or(RequestBodyRejection::UnsupportedMediaType(content_type))?;

        match content_type {
            MediaType::Form => Form::from_request(req, state)
                .await
                .map(|Form(value)| Self(value))
                .map_err(Into::into),
            MediaType::Json => Json::from_request(req, state)
                .await
                .map(|Json(value)| Self(value))
                .map_err(Into::into),
            MediaType::Rkyv => Rkyv::from_request(req, state)
                .await
                .map(|Rkyv(value)| Self(value))
                .map_err(Into::into),
        }
    }
}

/// Rejection for [`RequestBody`].
#[derive(Error, Debug)]
pub(super) enum RequestBodyRejection {
    /// `Content-Type` header was not set.
    #[error("\"Content-Type\" header must be set")]
    MissingContentType,

    /// `Content-Type` [`HeaderValue`](axum::http::HeaderValue) could not be converted to a string.
    #[error("error converting \"Content-Type\" header value to a string")]
    InvalidHeaderValue(#[from] header::ToStrError),

    /// Error parsing `Content-Type` header value as a [`Mime`] type.
    #[error("error parsing `{value}` from \"Content-Type\" header as a media type")]
    InvalidMediaType {
        /// Value attempted to parse.
        value: String,
        /// Error source.
        source: mime::FromStrError,
    },

    /// Given `Content-Type` is not supported.
    #[error("media type `{0}` is not supported for request bodies")]
    UnsupportedMediaType(Mime),

    /// Error deserializing [`Form`] data.
    #[error("error deserializing form data")]
    FormRejection(#[from] FormRejection),

    /// Error deserializing [`Json`] data.
    #[error("error deserializing JSON data")]
    JsonRejection(#[from] JsonRejection),

    /// Error deserializing [`Rkyv`] data.
    #[error("error deserializing rkyv data")]
    RkyvRejection(#[from] RkyvRejection),
}

impl IntoResponse for RequestBodyRejection {
    fn into_response(self) -> Response {
        match self {
            Self::MissingContentType | Self::UnsupportedMediaType(_) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, self.to_string()).into_response()
            }
            Self::InvalidHeaderValue(ref source) => {
                (StatusCode::BAD_REQUEST, format!("{self}: {source}")).into_response()
            }
            Self::InvalidMediaType { ref source, .. } => {
                (StatusCode::BAD_REQUEST, format!("{self}: {source}")).into_response()
            }
            Self::FormRejection(rejection) => rejection.into_response(),
            Self::JsonRejection(rejection) => rejection.into_response(),
            Self::RkyvRejection(rejection) => rejection.into_response(),
        }
    }
}

/// Response for serializing into a supported [`MediaType`].
pub(super) struct ResponseBody<T>(pub(super) ResponseContentType, pub(super) T);

impl<T> IntoResponse for ResponseBody<T>
where
    T: serde::Serialize
        + for<'a> rkyv::Serialize<HighSerializer<Vec<u8>, ArenaHandle<'a>, rancor::Error>>,
{
    fn into_response(self) -> Response {
        let Self(ResponseContentType(content_type), value) = self;

        match content_type {
            MediaType::Form => Form(value).into_response(),
            MediaType::Json => Json(value).into_response(),
            MediaType::Rkyv => Rkyv(value).into_response(),
        }
    }
}

/// Supported media types for a [`RequestBody`] or [`ResponseBody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaType {
    /// The `application/x-www-form-urlencoded` media type.
    Form,

    /// The `application/json` media type.
    Json,

    /// The `application/rkyv` media type.
    Rkyv,
}

impl MediaType {
    /// Media type for [`Form`](Self::Form).
    pub(super) const FORM: &str = "application/x-www-form-urlencoded";

    /// Media type for [`Json`](Self::Json).
    pub(super) const JSON: &str = "application/json";

    /// Media type for [`Rkyv`](Self::Rkyv).
    pub(super) const RKYV: &str = "application/rkyv";

    /// Return [`Some`] if the MIME type matches a supported [`MediaType`].
    fn try_from_mime(mime: &Mime) -> Option<Self> {
        match mime.essence_str() {
            Self::FORM => Some(Self::Form),
            Self::JSON => Some(Self::Json),
            Self::RKYV => Some(Self::Rkyv),
            _ => None,
        }
    }
}

/// Extractor for determining which media type a [`ResponseBody`] should be serialized to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResponseContentType(MediaType);

impl Default for ResponseContentType {
    fn default() -> Self {
        Self(MediaType::Json)
    }
}

impl ResponseContentType {
    /// Use this content type to construct a [`ResponseBody`].
    #[must_use]
    pub(super) const fn into_body<T>(self, body: T) -> ResponseBody<T> {
        ResponseBody(self, body)
    }
}

impl<S> FromRequestParts<S> for ResponseContentType {
    type Rejection = ResponseContentTypeRejection;

    #[expect(refining_impl_trait_internal, reason = "not public, needed for tests")]
    fn from_request_parts(parts: &mut Parts, _state: &S) -> Ready<Result<Self, Self::Rejection>> {
        let content_type = parts
            .headers
            .get_all(ACCEPT)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(|value| value.split(','))
                    .map_err(|source| ResponseContentTypeRejection::InvalidHeaderValue {
                        header: ACCEPT,
                        source,
                    })
            })
            .flatten_ok()
            .filter_map_ok(|value| {
                value.trim().parse::<Mime>().map_or_else(
                    |source| {
                        Some(Err(ResponseContentTypeRejection::InvalidMediaType {
                            value: value.to_owned(),
                            header: ACCEPT,
                            source,
                        }))
                    },
                    |mime| match mime
                        .get_param("q")
                        .map(|quality_value| quality_value.as_str().parse())
                        .transpose()
                    {
                        Ok(quality_value) => MediaType::try_from_mime(&mime).map_or_else(
                            || {
                                (mime == "*/*" || mime == "application/*").then(|| {
                                    Ok((
                                        Self::default(),
                                        quality_value.unwrap_or(QualityValue::MIN),
                                    ))
                                })
                            },
                            |media_type| {
                                Some(Ok((Self(media_type), quality_value.unwrap_or_default())))
                            },
                        ),
                        Err(source) => {
                            Some(Err(ResponseContentTypeRejection::QualityValueParseError {
                                mime,
                                source,
                            }))
                        }
                    },
                )
            })
            .map(Result::flatten)
            .process_results(|mut iter| {
                iter.fold_while(None, |candidate, (content_type, quality_value)| {
                    if quality_value == QualityValue::MAX {
                        FoldWhile::Done(Some((content_type, quality_value)))
                    } else if let Some((_, candidate_q)) = candidate {
                        if quality_value > candidate_q {
                            FoldWhile::Continue(Some((content_type, quality_value)))
                        } else {
                            FoldWhile::Continue(candidate)
                        }
                    } else {
                        FoldWhile::Continue(Some((content_type, quality_value)))
                    }
                })
            })
            .map(|fold| fold.into_inner().map(|(content_type, _)| content_type))
            .transpose()
            .or_else(|| {
                parts
                    .headers
                    .get(CONTENT_TYPE)?
                    .to_str()
                    .map_err(|source| ResponseContentTypeRejection::InvalidHeaderValue {
                        header: CONTENT_TYPE,
                        source,
                    })
                    .and_then(|value| {
                        value
                            .parse()
                            .map(|mime| MediaType::try_from_mime(&mime).map(Self))
                            .map_err(|source| ResponseContentTypeRejection::InvalidMediaType {
                                value: value.to_owned(),
                                header: CONTENT_TYPE,
                                source,
                            })
                    })
                    .transpose()
            })
            .unwrap_or_else(|| Ok(Self::default()));

        std::future::ready(content_type)
    }
}

/// Rejection for [`ResponseContentType`].
#[derive(Error, Debug)]
pub(super) enum ResponseContentTypeRejection {
    /// [`HeaderValue`](axum::http::HeaderValue) could not be represented as a string.
    #[error("error converting \"{header}\" header value to a string")]
    InvalidHeaderValue {
        /// Header whose value could not be converted.
        header: HeaderName,
        /// Error source.
        source: header::ToStrError,
    },

    /// Error parsing a [`Mime`] type.
    #[error("error parsing `{value}` from \"{header}\" header as a media type")]
    InvalidMediaType {
        /// Header value attempted to parse.
        value: String,
        /// Header whose value could not be parsed.
        header: HeaderName,
        /// Error source.
        source: mime::FromStrError,
    },

    /// Error parsing [`QualityValue`] from a media type's `q` parameter.
    #[error("error parsing quality value from \"accept\" header media type `{mime}`")]
    QualityValueParseError {
        /// Media type `q` value parsing failed with.
        mime: Mime,
        /// Error source.
        source: QualityValueParseError,
    },
}

impl IntoResponse for ResponseContentTypeRejection {
    fn into_response(self) -> Response {
        let message = match &self {
            Self::InvalidHeaderValue { source, .. } => format!("{self}: {source}"),
            Self::InvalidMediaType { source, .. } => format!("{self}: {source}"),
            Self::QualityValueParseError { source, .. } => format!("{self}: {source}"),
        };

        (StatusCode::BAD_REQUEST, message).into_response()
    }
}

/// Priority value of a media type in an [`ACCEPT`] header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct QualityValue(
    // Fractional value (d.ddd) encoded as number between 0 and 1,000.
    u16,
);

impl QualityValue {
    /// Minimum possible quality value.
    const MIN: Self = Self(0);

    /// Maximum possible quality value.
    const MAX: Self = Self(1_000);
}

impl Default for QualityValue {
    fn default() -> Self {
        Self::MAX
    }
}

impl FromStr for QualityValue {
    type Err = QualityValueParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "0" | "0." | "0.0" | "0.00" | "0.000" => Ok(Self::MIN),
            "1" | "1." | "1.0" | "1.00" | "1.000" => Ok(Self::MAX),
            s => {
                let digits = s
                    .strip_prefix("0.")
                    .ok_or_else(|| QualityValueParseError::InvalidStart(s.to_owned()))?;

                let multiplier = match digits.chars().count() {
                    0 => return Ok(Self(0_000)),
                    1 => 100,
                    2 => 10,
                    3 => 1,
                    _ => return Err(QualityValueParseError::MoreThanThreeDecimals),
                };

                digits
                    .parse()
                    .map(|value: u16| Self(value.saturating_mul(multiplier)))
                    .map_err(|source| QualityValueParseError::ParseDigitsError {
                        value: s.to_owned(),
                        source,
                    })
            }
        }
    }
}

/// Error returned when parsing a [`QualityValue`] from a string fails.
#[derive(Error, Debug, PartialEq, Eq)]
pub(super) enum QualityValueParseError {
    /// Quality value was not `0`, `1`, their equivalents, or start with `0.`.
    #[error(
        "quality value `{0}` was not `0`, `1`, their equivalents, or between the two (start with `0.`)"
    )]
    InvalidStart(String),

    /// Quality value had more than three characters after the decimal.
    #[error("quality values cannot have more than 3 decimal places")]
    MoreThanThreeDecimals,

    /// Error parsing digits after the decimal.
    // Normally, it is bad practice to include the source error's message in the encapsulating
    // error's message, but in this case it is used in `ResponseEncodingRejection::to_response()`.
    #[error("error parsing digits after the decimal from `{value}`: {source}")]
    ParseDigitsError {
        /// Value attempted to parse.
        value: String,
        /// Error returned when parsing the decimal digits failed.
        source: ParseIntError,
    },
}

/// [`rkyv`] extractor and response.
#[derive(Debug)]
pub(super) struct Rkyv<T>(T);

impl<T, S> FromRequest<S> for Rkyv<T>
where
    T: Archive,
    T::Archived: for<'a> CheckBytes<HighValidator<'a, rancor::BoxedError>>
        + rkyv::Deserialize<T, Strategy<Pool, rancor::BoxedError>>,
    S: Send + Sync,
{
    type Rejection = RkyvRejection;

    async fn from_request(req: Request, _state: &S) -> Result<Self, Self::Rejection> {
        if req
            .headers()
            .get(CONTENT_TYPE)
            .is_some_and(|value| value == MediaType::RKYV)
        {
            let mut stream = req.into_limited_body().into_data_stream();

            let (min, max) = Stream::size_hint(&stream);
            let capacity = max.unwrap_or(min);
            let mut buf = AlignedVec::<1024>::with_capacity(capacity);

            while let Some(bytes) = stream.try_next().await? {
                buf.extend_from_slice(&bytes);
            }

            Ok(Self(rkyv::from_bytes(&buf)?))
        } else {
            Err(RkyvRejection::MissingRkyvContentType)
        }
    }
}

impl<T> IntoResponse for Rkyv<T>
where
    T: for<'a> rkyv::Serialize<HighSerializer<Vec<u8>, ArenaHandle<'a>, rancor::Error>>,
{
    fn into_response(self) -> Response {
        let Self(value) = self;

        high::to_bytes_in(&value, Vec::with_capacity(128)).map_or_else(
            |error| {
                error!("error serializing response to rkyv: {error}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            },
            |bytes| ([(CONTENT_TYPE, MediaType::RKYV)], bytes).into_response(),
        )
    }
}

/// Rejection for [`Rkyv`].
#[derive(Error, Debug)]
pub(super) enum RkyvRejection {
    /// `Content-Type` header was not set to `application/rkyv`.
    #[error("expected request with `Content-Type: application/rkyv`")]
    MissingRkyvContentType,

    /// Request body was too large.
    #[error("failed to buffer the request body")]
    LengthLimitError(#[from] LengthLimitError),

    /// Unknown error while buffering the body.
    #[error("failed to buffer the request body")]
    UnknownBodyError(axum::Error),

    /// Error deserializing target type.
    #[error("error deserializing rkyv body into the target type")]
    RkyvError(#[from] rancor::BoxedError),
}

impl From<axum::Error> for RkyvRejection {
    fn from(error: axum::Error) -> Self {
        error
            .into_inner()
            .downcast::<axum::Error>()
            .map_or_else(|error| error, |error| error.into_inner())
            .downcast::<LengthLimitError>()
            .map_or_else(
                |error| Self::UnknownBodyError(axum::Error::new(error)),
                |error| (*error).into(),
            )
    }
}

impl IntoResponse for RkyvRejection {
    fn into_response(self) -> Response {
        match &self {
            Self::MissingRkyvContentType => (StatusCode::UNSUPPORTED_MEDIA_TYPE, self.to_string()),
            Self::LengthLimitError(source) => {
                (StatusCode::PAYLOAD_TOO_LARGE, format!("{self}: {source}"))
            }
            Self::UnknownBodyError(source) => {
                (StatusCode::BAD_REQUEST, format!("{self}: {source}"))
            }
            Self::RkyvError(source) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("{self}: {source}"),
            ),
        }
        .into_response()
    }
}

#[cfg(test)]
mod tests {
    use proptest::{prop_assert_eq, proptest};

    use super::*;

    #[cfg(test)]
    mod quality_value {
        use std::{
            fmt::{self, Display, Formatter},
            hint::black_box,
            ops::RangeInclusive,
        };

        use proptest::{
            arbitrary::Arbitrary,
            prop_assert,
            strategy::{Map, Strategy},
        };

        use super::*;

        impl Arbitrary for QualityValue {
            type Parameters = ();
            type Strategy = Map<RangeInclusive<u16>, fn(u16) -> Self>;

            fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
                (Self::MIN.0..=Self::MAX.0).prop_map(Self)
            }
        }

        impl Display for QualityValue {
            fn fmt(&self, f: &mut Formatter) -> fmt::Result {
                if *self == Self::MIN {
                    f.write_str("0")
                } else if *self == Self::MAX {
                    f.write_str("1")
                } else {
                    // Could be more concise (trim end 0s), but it's fine for testing.
                    write!(f, "0.{:03}", self.0)
                }
            }
        }

        proptest! {
            #[test]
            fn no_panic(quality_value: String) {
                drop(black_box(quality_value.parse::<QualityValue>()));
            }

            #[test]
            fn valid(quality_value: QualityValue) {
                if quality_value == QualityValue::MAX {
                    prop_assert_eq!(quality_value, "1".parse()?);
                    prop_assert_eq!(quality_value, "1.0".parse()?);
                    prop_assert_eq!(quality_value, "1.00".parse()?);
                    prop_assert_eq!(quality_value, "1.000".parse()?);
                } else if quality_value == QualityValue::MIN {
                    prop_assert_eq!(quality_value, "0".parse()?);
                    prop_assert_eq!(quality_value, "0.0".parse()?);
                    prop_assert_eq!(quality_value, "0.00".parse()?);
                    prop_assert_eq!(quality_value, "0.000".parse()?);
                } else {
                    let string = quality_value.to_string();
                    prop_assert_eq!(quality_value, string.parse()?);
                    prop_assert_eq!(quality_value, string.trim_end_matches('0').parse()?);
                }
            }

            #[test]
            fn invalid_start_err(quality_value in r"[1-9](?:\.[0-9]{0,2}[1-9])") {
                prop_assert_eq!(
                    quality_value.parse::<QualityValue>(),
                    Err(QualityValueParseError::InvalidStart(quality_value)),
                );
            }

            #[test]
            fn more_than_three_decimals_err(quality_value in r"0\.[0-9]{4,}") {
                prop_assert_eq!(
                    quality_value.parse::<QualityValue>(),
                    Err(QualityValueParseError::MoreThanThreeDecimals),
                );
            }

            #[test]
            fn parse_digits_err(quality_value in r"0\.[^0-9]{1,3}") {
                let result = quality_value.parse::<QualityValue>();
                prop_assert!(
                    matches!(result, Err(QualityValueParseError::ParseDigitsError{..})),
                    "parse of `{quality_value}` didn't error with `ParseDigitsError`, \
                     result: {result:?}",
                );
            }
        }
    }

    mod response_content_type {
        use std::error::Error;

        use super::*;

        #[test]
        fn json_default() -> Result<(), Box<dyn Error>> {
            let (mut parts, ()) = Request::<()>::default().into_parts();

            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Json),
            );

            parts.headers.insert(ACCEPT, "*/*".parse()?);
            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Json),
            );

            parts.headers.insert(ACCEPT, "application/*".parse()?);
            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Json),
            );

            parts.headers.insert(ACCEPT, "text/plain".parse()?);
            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Json),
            );

            Ok(())
        }

        #[test]
        fn specific_wins() -> Result<(), Box<dyn Error>> {
            let (mut parts, ()) = Request::<()>::default().into_parts();
            parts
                .headers
                .insert(ACCEPT, format!("*/*, {}", MediaType::RKYV).parse()?);

            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Rkyv),
            );

            Ok(())
        }

        #[test]
        fn first_wins() -> Result<(), Box<dyn Error>> {
            let (mut parts, ()) = Request::<()>::default().into_parts();
            parts.headers.insert(
                ACCEPT,
                format!("{}, {}", MediaType::RKYV, MediaType::JSON).parse()?,
            );

            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Rkyv),
            );

            Ok(())
        }

        #[test]
        fn content_type_fallback() -> Result<(), Box<dyn Error>> {
            let (mut parts, ()) = Request::<()>::default().into_parts();
            parts.headers.insert(CONTENT_TYPE, MediaType::RKYV.parse()?);

            assert_eq!(
                ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                ResponseContentType(MediaType::Rkyv),
            );

            Ok(())
        }

        proptest! {
            #[test]
            fn highest_quality_value_wins(
                form_q: QualityValue,
                json_q: QualityValue,
                rkyv_q: QualityValue,
            ) {
                let (mut parts, ()) = Request::<()>::default().into_parts();
                let accept = format!(
                    "{};q={form_q}, {};q={json_q}, {};q={rkyv_q}",
                    MediaType::FORM,
                    MediaType::JSON,
                    MediaType::RKYV
                );
                parts.headers.insert(ACCEPT, accept.parse()?);

                let expected = if form_q >= json_q && form_q >= rkyv_q {
                    ResponseContentType(MediaType::Form)
                } else if json_q > form_q && json_q >= rkyv_q {
                    ResponseContentType(MediaType::Json)
                } else {
                    ResponseContentType(MediaType::Rkyv)
                };

                prop_assert_eq!(
                    ResponseContentType::from_request_parts(&mut parts, &()).into_inner()?,
                    expected,
                );
            }
        }
    }

    mod rkyv {
        use axum::{body::Body, extract::DefaultBodyLimit, handler::Handler, http};

        use super::*;

        #[tokio::test]
        async fn length_limit_err() -> Result<(), http::Error> {
            const LENGTH_LIMIT: usize = 64;

            let handler = |body: Result<Rkyv<()>, RkyvRejection>| std::future::ready(body);
            let handler = handler.layer(DefaultBodyLimit::max(LENGTH_LIMIT));

            let request = Request::builder()
                .header(CONTENT_TYPE, MediaType::RKYV)
                .body(Body::from(vec![0; LENGTH_LIMIT + 1]))?;

            let response = handler.call(request, ()).await;

            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

            Ok(())
        }
    }
}
