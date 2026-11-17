use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nexus_common::NexusError;
use serde_json::json;

/// Wraps [`NexusError`] so handlers can use `?` and return JSON errors:
/// `{ "error": { "code": "...", "message": "..." } }`.
#[derive(Debug)]
pub struct ApiError(pub NexusError);

pub type ApiResult<T> = Result<T, ApiError>;

impl<E: Into<NexusError>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let e = &self.0;
        let status = match e {
            NexusError::NotFound(_) => StatusCode::NOT_FOUND,
            NexusError::InvalidInput(_) | NexusError::PathRejected(_) => StatusCode::BAD_REQUEST,
            NexusError::Conflict(_) => StatusCode::CONFLICT,
            NexusError::PermissionDenied(_) => StatusCode::FORBIDDEN,
            NexusError::Unauthorized => StatusCode::UNAUTHORIZED,
            NexusError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            NexusError::InsufficientStorage(_) => StatusCode::INSUFFICIENT_STORAGE,
            NexusError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            NexusError::PeerUnreachable(_) => StatusCode::BAD_GATEWAY,
            NexusError::Io(io) => match io.kind() {
                std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            NexusError::Database(_) | NexusError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() && status != StatusCode::BAD_GATEWAY {
            tracing::error!(error = %e, "request failed");
        }
        let message = match e {
            NexusError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                "Windows denied access to this item".to_string()
            }
            NexusError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                "The item no longer exists on disk".to_string()
            }
            other => other.to_string(),
        };
        (status, Json(json!({ "error": { "code": e.code(), "message": message } }))).into_response()
    }
}
