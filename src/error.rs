use actix_web::{HttpResponse, ResponseError};
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    /// Malformed request body or invalid parameters
    InvalidRequest(String),
    /// Upstream LLM API is unreachable
    UpstreamUnavailable(String),
    /// Upstream returned an error status
    UpstreamError { status: u16, body: String },
    /// Configuration loading failure
    ConfigError(String),
    /// JSON serialization/deserialization failure
    JsonError(String),
    /// Internal proxy error
    Internal(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
            AppError::UpstreamUnavailable(msg) => write!(f, "upstream unavailable: {msg}"),
            AppError::UpstreamError { status, body } => {
                write!(f, "upstream error {status}: {body}")
            }
            AppError::ConfigError(msg) => write!(f, "configuration error: {msg}"),
            AppError::JsonError(msg) => write!(f, "json error: {msg}"),
            AppError::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl ResponseError for AppError {
    fn error_response(&self) -> HttpResponse {
        let (status, error_type, message) = match self {
            AppError::InvalidRequest(msg) => (
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_request_error",
                msg.clone(),
            ),
            AppError::UpstreamUnavailable(_) => (
                actix_web::http::StatusCode::BAD_GATEWAY,
                "proxy_error",
                self.to_string(),
            ),
            AppError::UpstreamError { status, body } => {
                let code = actix_web::http::StatusCode::from_u16(*status)
                    .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR);
                (code, "upstream_error", body.clone())
            }
            AppError::ConfigError(_) => (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "config_error",
                self.to_string(),
            ),
            AppError::JsonError(msg) => (
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_request_error",
                msg.clone(),
            ),
            AppError::Internal(_) => (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                self.to_string(),
            ),
        };

        HttpResponse::build(status).json(serde_json::json!({
            "error": {
                "type": error_type,
                "message": message,
            }
        }))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::JsonError(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_connect() || e.is_timeout() {
            AppError::UpstreamUnavailable(e.to_string())
        } else {
            AppError::Internal(e.to_string())
        }
    }
}
