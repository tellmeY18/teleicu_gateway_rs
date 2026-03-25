use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("camera locked — another operation is in progress")]
    CameraLocked,

    #[error("invalid camera credentials")]
    InvalidCameraCredentials,

    #[error("onvif error: {0}")]
    Onvif(String),

    #[error("care api error: {0}")]
    CareApi(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::CameraLocked => (StatusCode::CONFLICT, self.to_string()),
            AppError::InvalidCameraCredentials => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Onvif(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::CareApi(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::Db(_) => {
                tracing::error!("Database error: {self}");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error".into())
            }
            AppError::Internal(_) => {
                tracing::error!("Internal error: {self}");
                sentry::capture_error(&self);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };

        let body = axum::Json(json!({ "error": message }));
        (status, body).into_response()
    }
}
