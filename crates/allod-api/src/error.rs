use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// API error, rendered as `{"error": {"code", "message"}}` (SPEC §11.2).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "FORBIDDEN", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "CONFLICT", message)
    }
    pub fn too_many(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", message)
    }
}

impl From<allod_db::DbError> for ApiError {
    fn from(e: allod_db::DbError) -> Self {
        match e {
            allod_db::DbError::Constraint(m) => ApiError::conflict(m),
            other => {
                tracing::error!(error = %other, "database error");
                ApiError::internal("database error")
            }
        }
    }
}

impl From<allod_ln::BackendError> for ApiError {
    fn from(e: allod_ln::BackendError) -> Self {
        use allod_ln::BackendError as B;
        match &e {
            B::InvalidRequest(_) => ApiError::bad_request(e.to_string()),
            B::Lightning(_) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "LIGHTNING", e.to_string())
            }
            B::Permission(_) => {
                ApiError::new(StatusCode::FORBIDDEN, "NODE_PERMISSION", e.to_string())
            }
            B::Unsupported(_) => {
                ApiError::new(StatusCode::NOT_IMPLEMENTED, "NOT_SUPPORTED", e.to_string())
            }
            B::Unavailable(_) | B::Timeout | B::Credentials(_) => {
                ApiError::new(StatusCode::BAD_GATEWAY, "NODE_UNAVAILABLE", e.to_string())
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": {"code": self.code, "message": self.message}})))
            .into_response()
    }
}
