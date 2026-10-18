use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use meisync_core::{Credentials, FileId, PermissionSet, UserId, Visibility};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{ApiError, FileInfo};
use crate::extract::AuthUser;
use crate::service::{Depth, FileService};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/files", get(get_files_root).put(put_file_root))
        .route("/files/", get(get_files_root).put(put_file_root))
        .route(
            "/files/{*path}",
            get(get_files).put(put_file).delete(delete_file),
        )
        .route("/acl/{id}", get(get_acl).post(set_acl).delete(delete_acl))
        .route("/document", get(get_document_root))
        .route("/document/{*path}", get(get_document))
}

/// The control plane: JSON view over the very same FileService the protocols use.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok", "service": "meisync"}))
}

pub async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let documents = state.documents.status().await;
    Json(serde_json::json!({
        "status": "ok",
        "data_dir": state.config.data_dir,
        "documents": documents,
        "public_default_write": state.config.public_default_write,
        "max_body_bytes": state.config.max_body_bytes,
        "compaction": {
            "max_updates": state.config.compaction().max_updates,
            "max_update_bytes": state.config.compaction().max_update_bytes,
            "snapshot_bytes": state.config.compaction().snapshot_bytes,
        },
    }))
}

pub async fn register(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> Result<(axum::http::StatusCode, Json<meisync_core::AuthToken>), ApiError> {
    let token = state
        .auth
        .register(&credentials.username, &credentials.password)
        .await?;
    // Provision the personal namespace + bucket (§4, §30). A failure rolls the account back: a
    // half-created user could neither log in nor register again, so the name would be lost.
    if let Err(err) = state
        .files
        .ensure_user_namespace(&token.user.id, &token.user.username)
        .await
    {
        if let Err(rollback) = state.auth.rollback_registration(&token.user.id).await {
            tracing::warn!(error = %rollback, "failed to roll back a half-created account");
        }
        return Err(ApiError(err));
    }
    Ok((axum::http::StatusCode::CREATED, Json(token)))
}

pub async fn login(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> Result<Json<meisync_core::AuthToken>, ApiError> {
    let token = state
        .auth
        .login(&credentials.username, &credentials.password)
        .await?;
    state
        .files
        .ensure_user_namespace(&token.user.id, &token.user.username)
        .await?;
    Ok(Json(token))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::http::StatusCode, ApiError> {
    if let Some(token) = crate::extract::token_from_headers(&headers) {
        state.auth.logout(&token).await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn me(AuthUser(user): AuthUser) -> Json<meisync_core::PublicUser> {
    Json(user.to_public())
}

#[derive(Debug, Deserialize, Default)]
pub struct PutQuery {
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    /// `0` returns the node itself, `1` (default for collections) adds its children,
    /// `infinity` walks the whole subtree.
    pub depth: Option<String>,
}

/// `GET /api/files[/<path>][?depth=n]`
///
/// Without `depth` a single node is returned, with `depth` the node plus its descendants.
pub async fn get_files_root(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    get_files_for(state, user, query, String::new()).await
}

pub async fn get_files(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<ListQuery>,
    Path(path): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    get_files_for(state, user, query, path).await
}

async fn get_files_for(
    state: AppState,
    user: meisync_core::User,
    query: ListQuery,
    path: String,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = if path.is_empty() || path == "/" {
        "/".to_string()
    } else {
        path
    };
    let meta = state.files.get(&user, &path).await?;
    let meta_path = state.files.path_of(&meta.id).await?;

    let depth = match query.depth.as_deref() {
        None => {
            return Ok(Json(serde_json::json!(FileInfo::from_metadata(
                &meta, meta_path
            ))))
        }
        Some(value) => Depth::parse(value),
    };

    let mut out = vec![FileInfo::from_metadata(&meta, meta_path)];
    if depth != Depth::Zero {
        for child in state.files.list(&user, &path, depth).await? {
            let child_path = state.files.path_of(&child.id).await?;
            out.push(FileInfo::from_metadata(&child, child_path));
        }
    }
    Ok(Json(serde_json::json!(out)))
}

pub async fn put_file_root(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<PutQuery>,
    body: axum::body::Bytes,
) -> Result<Json<FileInfo>, ApiError> {
    put_file_for(state, user, query, String::new(), body).await
}

pub async fn put_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<PutQuery>,
    Path(path): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<FileInfo>, ApiError> {
    put_file_for(state, user, query, path, body).await
}

async fn put_file_for(
    state: AppState,
    user: meisync_core::User,
    query: PutQuery,
    path: String,
    body: axum::body::Bytes,
) -> Result<Json<FileInfo>, ApiError> {
    if path.is_empty() || path == "/" {
        return Err(ApiError(meisync_core::MeiSyncError::bad_request(
            "a file name is required",
        )));
    }
    let (parent, name) = crate::service::DefaultFileService::split_parent(&path)?;
    let visibility = match query.visibility.as_deref() {
        Some("public") => Some(meisync_core::Visibility::Public),
        Some("private") | None => None,
        Some(other) => {
            return Err(ApiError(meisync_core::MeiSyncError::bad_request(format!(
                "unknown visibility: {other}"
            ))))
        }
    };
    let spec = crate::service::CreateFileSpec {
        parent_path: parent,
        name,
        kind: meisync_core::FileKind::File,
        content_type: None,
        visibility,
        initial_content: Some(body.to_vec()),
    };
    let meta = match state.files.create(&user, spec).await {
        Ok(meta) => meta,
        Err(err) => {
            // Updating an existing file through the API replaces its content.
            if !matches!(err, meisync_core::MeiSyncError::AlreadyExists(_)) {
                return Err(ApiError(err));
            }
            state
                .files
                .replace_content(&user, &path, body.to_vec(), None)
                .await?;
            state.files.get(&user, &path).await?
        }
    };
    let meta_path = state.files.path_of(&meta.id).await?;
    Ok(Json(FileInfo::from_metadata(&meta, meta_path)))
}

/// `GET /api/document[/<path>]` – the whole `FileDocument` aggregate (§3.1):
/// metadata plus the content materialised from the YDoc.
pub async fn get_document_root(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    match state.files.document(&user, "/").await {
        Ok(document) => Ok(Json(serde_json::json!(document_json(&document, "/")?))),
        // The namespace root is a collection, not a document.
        Err(meisync_core::MeiSyncError::Conflict(_)) => Ok(Json(serde_json::json!({
            "path": "/",
            "collection": true,
        }))),
        Err(err) => Err(ApiError(err)),
    }
}

pub async fn get_document(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(path): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = if path.is_empty() || path == "/" {
        "/".to_string()
    } else {
        path
    };
    let document = state.files.document(&user, &path).await?;
    let canonical = state.files.path_of(&document.id).await?;
    Ok(Json(document_json(&document, &canonical)?))
}

fn document_json(document: &meisync_core::FileDocument, path: &str) -> Result<serde_json::Value, ApiError> {
    Ok(serde_json::json!({
        "id": document.id.0,
        "path": path,
        "collection": document.metadata.is_collection(),
        "metadata": FileInfo::from_metadata(&document.metadata, path.to_string()),
        "content": document.materialize_string(),
    }))
}

pub async fn delete_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(path): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    state.files.delete(&user, &path).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn get_acl(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let entries = state.files.acl_list(&user, &FileId::new(id)).await?;
    let out = entries
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "file_id": entry.file_id.0,
                "user_id": entry.user_id.0,
                "permissions": entry.permissions.to_list(),
            })
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AclRequest {
    pub username: Option<String>,
    pub user_id: Option<String>,
    pub permissions: Vec<String>,
    pub visibility: Option<String>,
}

pub async fn set_acl(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
    Json(request): Json<AclRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    let file_id = FileId::new(id);
    if let Some(visibility) = request.visibility {
        let visibility = match visibility.as_str() {
            "public" => Visibility::Public,
            "private" => Visibility::Private,
            other => {
                return Err(ApiError(meisync_core::MeiSyncError::bad_request(format!(
                    "unknown visibility: {other}"
                ))))
            }
        };
        state.files.set_visibility(&user, &file_id, visibility).await?;
    }
    if let Some(target) = request.user_id.or(request.username) {
        let target_id = match state.auth.users().find_by_username(&target).await? {
            Some(found) => found.id,
            None => UserId::new(target.clone()),
        };
        state
            .files
            .acl_grant(&user, &file_id, &target_id, PermissionSet::from_list(&request.permissions))
            .await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn delete_acl(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let file_id = FileId::new(id);
    let target = user.id.clone();
    state
        .files
        .acl_grant(&user, &file_id, &target, PermissionSet::NONE)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Small helper used by the protocol crates to convert a `HeaderMap` into a map.
pub fn header_map(headers: &axum::http::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|value| (k.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}