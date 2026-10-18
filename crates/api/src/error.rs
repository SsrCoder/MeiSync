use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use meisync_core::{FileMetadata, MeiSyncError};

/// Uniform error type for every HTTP surface (API, WebDAV, S3).
#[derive(Debug)]
pub struct ApiError(pub MeiSyncError);

impl From<MeiSyncError> for ApiError {
    fn from(value: MeiSyncError) -> Self {
        ApiError(value)
    }
}

impl From<ApiError> for MeiSyncError {
    fn from(value: ApiError) -> Self {
        value.0
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Body returned for JSON APIs. WebDAV / S3 render errors with their own format.
#[derive(serde::Serialize)]
struct ErrorBody {
    error: String,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = ErrorBody {
            error: kind(&self.0).to_string(),
            message: self.0.to_string(),
        };
        // `axum::Json` already sets `content-type: application/json` for the error body.
        (status, axum::Json(body)).into_response()
    }
}

fn kind(err: &MeiSyncError) -> &'static str {
    match err {
        MeiSyncError::NotFound(_) => "not_found",
        MeiSyncError::Conflict(_) | MeiSyncError::AlreadyExists(_) => "conflict",
        MeiSyncError::Unauthorized(_) => "unauthorized",
        MeiSyncError::Forbidden(_) => "forbidden",
        MeiSyncError::BadRequest(_) => "bad_request",
        MeiSyncError::PreconditionFailed(_) => "precondition_failed",
        MeiSyncError::NotImplemented(_) => "not_implemented",
        MeiSyncError::MethodNotAllowed(_) => "method_not_allowed",
        MeiSyncError::Storage(_) | MeiSyncError::Document(_) | MeiSyncError::Serde(_) => "storage_error",
        MeiSyncError::Internal(_) => "internal_error",
    }
}

/// WebDAV needs to map errors onto DAV specific status codes.
pub fn status_of(err: &MeiSyncError) -> StatusCode {
    match err {
        MeiSyncError::Conflict(_) | MeiSyncError::AlreadyExists(_) => StatusCode::CONFLICT,
        MeiSyncError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        MeiSyncError::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
        MeiSyncError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        MeiSyncError::Forbidden(_) => StatusCode::FORBIDDEN,
        MeiSyncError::NotFound(_) => StatusCode::NOT_FOUND,
        MeiSyncError::PreconditionFailed(_) => StatusCode::PRECONDITION_FAILED,
        MeiSyncError::BadRequest(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Helper: a `403 Forbidden` for a file that must not even be visible.
pub fn forbidden_path(path: &str) -> MeiSyncError {
    MeiSyncError::NotFound(format!("no such file: {path}"))
}

/// Description of a file for JSON responses.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileInfo {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub path: String,
    pub owner_id: String,
    pub visibility: String,
    pub content_type: String,
    pub kind: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub size: u64,
    pub etag: Option<String>,
    pub version: i64,
    pub sync_mode: String,
}

impl FileInfo {
    pub fn from_metadata(meta: &FileMetadata, path: String) -> Self {
        Self {
            id: meta.id.0.clone(),
            parent_id: meta.parent_id.as_ref().map(|p| p.0.clone()),
            name: meta.name.clone(),
            path,
            owner_id: meta.owner_id.0.clone(),
            visibility: meta.visibility.as_str().to_string(),
            content_type: meta
                .content_type
                .clone()
                .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name)),
            kind: meta.kind.as_str().to_string(),
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            size: meta.size,
            etag: meta.etag.clone(),
            version: meta.version,
            sync_mode: meta.sync_mode.as_str().to_string(),
        }
    }
}