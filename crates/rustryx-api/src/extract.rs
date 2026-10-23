//! Request extractors that fail with the error envelope instead of axum's
//! plain-text rejections.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use http::StatusCode;
use http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// JSON body. Syntax and shape errors are 400, a missing JSON content type is
/// 415 and a body over the route's limit is 413, all as the envelope.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_error(&rejection)),
        }
    }
}

fn json_error(rejection: &JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "The request must have Content-Type: application/json",
        ),
        JsonRejection::JsonSyntaxError(_) => {
            ApiError::bad_request("invalid_json", "The request body is not valid JSON")
        }
        // The serde message names the offending field; it describes the caller's
        // own input and carries nothing internal.
        JsonRejection::JsonDataError(_) => {
            ApiError::bad_request("invalid_json", rejection.body_text())
        }
        // axum classifies an over-limit body itself (status 413); anything else
        // (a stalled or broken upload) is looked up in the error chain. Relying on
        // the chain alone misses the limit error once axum has wrapped it.
        JsonRejection::BytesRejection(inner) if inner.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            ApiError::payload_too_large()
        }
        JsonRejection::BytesRejection(_) => ApiError::from_body_error(rejection),
        _ => ApiError::bad_request("invalid_json", "The request body could not be processed"),
    }
}

/// Query string, as the envelope on failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(query_error(&rejection)),
        }
    }
}

fn query_error(rejection: &QueryRejection) -> ApiError {
    ApiError::bad_request("invalid_query", rejection.body_text())
}

/// Path parameters, as the envelope on failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(path_error(&rejection)),
        }
    }
}

fn path_error(rejection: &PathRejection) -> ApiError {
    match rejection {
        // A route declared with more parameters than the handler extracts is a
        // programming error, not something the client can fix.
        PathRejection::MissingPathParams(_) => {
            ApiError::internal(format!("path extraction misconfigured: {rejection}"))
        }
        _ => ApiError::bad_request("invalid_path", rejection.body_text()),
    }
}
