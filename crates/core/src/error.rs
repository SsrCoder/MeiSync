use thiserror::Error;

/// The single error type used across every MeiSync crate.
#[derive(Debug, Error)]
pub enum MeiSyncError {
    #[error("resource not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("already exists: {0}")]
    AlreadyExists(String),

    /// HTTP 401 – no/invalid credentials.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// HTTP 403 – authenticated but not allowed.
    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    /// HTTP 412 – `If-Match` / `If-None-Match` did not hold.
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),

    /// HTTP 405 – e.g. `MKCOL` on an already existing resource (RFC 4918 §8.3.1).
    #[error("method not allowed: {0}")]
    MethodNotAllowed(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("document error: {0}")]
    Document(String),

    #[error("serialization error: {0}")]
    Serde(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl MeiSyncError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    /// Rough HTTP status mapping, used by every protocol frontend.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::AlreadyExists(_) => 409,
            Self::Unauthorized(_) => 401,
            Self::Forbidden(_) => 403,
            Self::BadRequest(_) => 400,
            Self::PreconditionFailed(_) => 412,
            Self::MethodNotAllowed(_) => 405,
            Self::NotImplemented(_) => 501,
            Self::Storage(_) | Self::Document(_) | Self::Serde(_) | Self::Internal(_) => 500,
        }
    }
}

impl From<std::io::Error> for MeiSyncError {
    fn from(value: std::io::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_json::Error> for MeiSyncError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

pub type Result<T, E = MeiSyncError> = std::result::Result<T, E>;