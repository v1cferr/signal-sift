//! One error envelope for every endpoint:
//! `{"error": {"code": "not_found", "message": "..."}}`.
//!
//! `code` is a stable machine-readable string clients may branch on; `message`
//! is for humans and may change. Internal details are logged, never returned.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use signalsift_storage::StorageError;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    PayloadTooLarge,
    Unauthorized,
    NotFound(String),
    Internal(String),
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: Body<'a>,
}

#[derive(Serialize)]
struct Body<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.as_str()),
            ApiError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body exceeds 16 KiB",
            ),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or invalid bearer token",
            ),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m.as_str()),
            ApiError::Internal(detail) => {
                tracing::error!(%detail, "internal API error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error; see daemon logs",
                )
            }
        };
        (
            status,
            Json(Envelope {
                error: Body { code, message },
            }),
        )
            .into_response()
    }
}

/// Malformed or oversized JSON bodies use the common envelope too, instead
/// of the framework's plain-text rejection.
impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        if r.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge
        } else {
            ApiError::BadRequest(r.body_text())
        }
    }
}

impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        ApiError::Internal(e.to_string())
    }
}
