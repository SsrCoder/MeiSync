//! `/api/admin` – the administration control plane.
//!
//! Every handler is protected by [`AdminUser`]; the console itself is a small embedded SPA served
//! from `/admin`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{request::Parts, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use meisync_api::extract::AuthToken;
use meisync_api::{ApiError, AppState, Depth, FileInfo, FileService};
use meisync_core::{Credentials, MeiSyncError, PublicUser, User, UserId};
use serde::{Deserialize, Serialize};

use crate::repo::{
    directory_size, AdminDeviceRow, AdminRepository, AdminSessionRow, AdminStats, AdminUserRow,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/overview", get(overview))
        .route("/status", get(overview))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", patch(update_user).delete(delete_user))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", delete(revoke_session))
        .route("/devices", get(list_devices))
        .route("/devices/{user_id}/{device_id}", delete(delete_device))
        .route("/audit", get(list_audit))
        .route("/files", get(list_files).delete(delete_file))
        .route("/file", get(get_file))
}

/// Authenticated account with the `admin` role.
#[derive(Debug, Clone)]
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // The admin API deliberately does not accept `?token=`: admin tokens should never end up
        // in browser history, proxy logs or a copied URL.
        let token = AuthToken::from_parts(parts)
            .ok_or_else(|| ApiError(MeiSyncError::Unauthorized("missing bearer token".into())))?;
        let user = state.auth.authenticate(&token).await?;
        if !state.auth.is_admin(&user.id).await.map_err(ApiError)? {
            return Err(ApiError(MeiSyncError::Forbidden(
                "administrator access required".into(),
            )));
        }
        Ok(AdminUser(user))
    }
}

fn repo(state: &AppState) -> AdminRepository {
    AdminRepository::new(state.storage.db.clone())
}

async fn audit(
    state: &AppState,
    actor: Option<&User>,
    action: &str,
    target: Option<String>,
    detail: Option<String>,
) {
    let repo = repo(state);
    if let Err(err) = repo
        .record_audit(
            actor.map(|u| u.id.0.clone()),
            actor.map(|u| u.username.clone()),
            action,
            target,
            detail,
        )
        .await
    {
        tracing::warn!(error = %err, action, "failed to write audit log entry");
    }
}

// ---------------------------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AdminAuth {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub user: PublicUser,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct AdminMe {
    pub user: PublicUser,
    pub role: String,
    pub permissions: Vec<String>,
}

pub async fn login(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> Result<Json<AdminAuth>, ApiError> {
    let result = match state
        .auth
        .login(&credentials.username, &credentials.password)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            audit(
                &state,
                None,
                "admin.login.failed",
                Some(credentials.username.clone()),
                Some("invalid credentials".into()),
            )
            .await;
            return Err(ApiError(err));
        }
    };
    let is_admin = state.auth.is_admin(&result.user.id).await?;
    if !is_admin {
        let _ = state.auth.logout(&result.token).await;
        audit(
            &state,
            None,
            "admin.login.failed",
            Some(credentials.username.clone()),
            Some("account is not an administrator".into()),
        )
        .await;
        return Err(ApiError(MeiSyncError::Forbidden(
            "administrator access required".into(),
        )));
    }
    let user = state
        .auth
        .users()
        .find_by_id(&result.user.id)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::Unauthorized("unknown user".into())))?;
    audit(
        &state,
        Some(&user),
        "admin.login",
        Some(user.username.clone()),
        None,
    )
    .await;
    Ok(Json(AdminAuth {
        token: result.token,
        expires_at: result.expires_at,
        user: result.user,
        role: "admin".into(),
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    AdminUser(user): AdminUser,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    if let Some(token) = meisync_api::extract::token_from_headers(&headers) {
        state.auth.logout(&token).await?;
    }
    audit(&state, Some(&user), "admin.logout", None, None).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(State(state): State<AppState>, AdminUser(user): AdminUser) -> Json<AdminMe> {
    let role = state
        .auth
        .flags(&user.id)
        .await
        .map(|flags| flags.role)
        .unwrap_or_else(|_| "admin".into());
    Json(AdminMe {
        user: user.to_public(),
        role,
        permissions: vec![
            "users.read".into(),
            "users.write".into(),
            "files.read".into(),
            "files.write".into(),
            "sessions.write".into(),
            "devices.write".into(),
            "status.read".into(),
        ],
    })
}

// ---------------------------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Overview {
    pub stats: AdminStats,
    pub documents: meisync_document::DocumentStatus,
    pub server: ServerView,
    pub disk: DiskView,
    pub recent_audit: Vec<meisync_persistence::AuditRecord>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ServerView {
    pub version: String,
    pub data_dir: String,
    pub bind: String,
    pub public_default_write: bool,
    pub max_body_bytes: usize,
    pub token_ttl_secs: u64,
    pub compaction: CompactionView,
}

#[derive(Debug, Serialize)]
pub struct CompactionView {
    pub max_updates: u64,
    pub max_update_bytes: u64,
    pub snapshot_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskView {
    pub documents_bytes: u64,
    pub database_bytes: u64,
    pub data_dir_bytes: u64,
}

pub async fn overview(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
) -> Result<Json<Overview>, ApiError> {
    let repo = repo(&state);
    let stats = repo.stats().await?;
    let documents = state.documents.status().await;
    let recent_audit = repo.list_audit(20).await?;
    let data_dir = state.config.data_dir.clone();
    let documents_dir = data_dir.join(meisync_persistence::DOCS_DIR_NAME);
    let db_file = data_dir.join(meisync_persistence::DATABASE_FILE_NAME);
    let (documents_bytes, database_bytes, data_dir_bytes) =
        tokio::task::spawn_blocking(move || {
            (
                directory_size(&documents_dir),
                std::fs::metadata(&db_file).map(|m| m.len()).unwrap_or(0),
                directory_size(&data_dir),
            )
        })
        .await
        .unwrap_or((0, 0, 0));
    let config = &state.config;
    Ok(Json(Overview {
        stats,
        documents,
        server: ServerView {
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: config.data_dir.display().to_string(),
            bind: config.bind.clone(),
            public_default_write: config.public_default_write,
            max_body_bytes: config.max_body_bytes,
            token_ttl_secs: config.token_ttl.as_secs(),
            compaction: CompactionView {
                max_updates: config.compaction().max_updates,
                max_update_bytes: config.compaction().max_update_bytes,
                snapshot_bytes: config.compaction().snapshot_bytes,
            },
        },
        disk: DiskView {
            documents_bytes,
            database_bytes,
            data_dir_bytes,
        },
        recent_audit,
        capabilities: vec![
            "webdav".into(),
            "s3".into(),
            "ysync".into(),
            "obsidian-sync".into(),
            "admin-console".into(),
        ],
    }))
}

// ---------------------------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct UsersQuery {
    pub search: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct UserList {
    pub users: Vec<AdminUserRow>,
    pub total: i64,
    pub limit: usize,
    pub offset: usize,
}

pub async fn list_users(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<UsersQuery>,
) -> Result<Json<UserList>, ApiError> {
    let repo = repo(&state);
    let limit = query.limit.unwrap_or(100);
    let offset = query.offset.unwrap_or(0);
    let users = repo.list_users(query.search, limit, offset).await?;
    let total = repo.stats().await?.users;
    Ok(Json(UserList {
        users,
        total,
        limit,
        offset,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
    pub role: Option<String>,
    pub disabled: Option<bool>,
}

pub async fn create_user(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Json(request): Json<CreateUser>,
) -> Result<(StatusCode, Json<AdminUserRow>), ApiError> {
    let role = match request.role.as_deref().unwrap_or("user") {
        "admin" => "admin",
        "user" => "user",
        other => {
            return Err(ApiError(MeiSyncError::bad_request(format!(
                "unknown role: {other}"
            ))))
        }
    };
    let token = state
        .auth
        .register(&request.username, &request.password)
        .await?;
    if let Err(err) = state
        .files
        .ensure_user_namespace(&token.user.id, &token.user.username)
        .await
    {
        let _ = state.auth.rollback_registration(&token.user.id).await;
        return Err(ApiError(err));
    }
    // The token issued by `register` is not handed to the administrator: the account has to log
    // in itself.
    let _ = state.auth.logout(&token.token).await;
    state.auth.users().set_role(&token.user.id, role).await?;
    if request.disabled.unwrap_or(false) {
        state
            .auth
            .users()
            .set_disabled(&token.user.id, true)
            .await?;
    }
    audit(
        &state,
        Some(&actor),
        "admin.user.create",
        Some(token.user.username.clone()),
        Some(format!("role={role}")),
    )
    .await;
    let row = repo(&state)
        .find_user(&token.user.id.0)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::internal("created user disappeared")))?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateUser {
    pub role: Option<String>,
    pub disabled: Option<bool>,
    pub password: Option<String>,
}

pub async fn update_user(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Path(id): Path<String>,
    Json(request): Json<UpdateUser>,
) -> Result<Json<AdminUserRow>, ApiError> {
    let user_id = UserId::new(id);
    let target = state
        .auth
        .users()
        .find_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::NotFound(user_id.0.clone())))?;
    let flags = state.auth.flags(&user_id).await?;
    let admin_count = state.auth.users().count_admins().await?;

    if let Some(role) = request.role.as_deref() {
        if role != "admin" && role != "user" {
            return Err(ApiError(MeiSyncError::bad_request(format!(
                "unknown role: {role}"
            ))));
        }
        if flags.role == "admin" && role != "admin" && admin_count <= 1 && !flags.disabled {
            return Err(ApiError(MeiSyncError::bad_request(
                "cannot remove the last administrator",
            )));
        }
        state.auth.users().set_role(&user_id, role).await?;
    }

    if let Some(disabled) = request.disabled {
        if disabled && flags.role == "admin" && admin_count <= 1 && !flags.disabled {
            return Err(ApiError(MeiSyncError::bad_request(
                "cannot disable the last administrator",
            )));
        }
        if disabled && actor.id == user_id {
            return Err(ApiError(MeiSyncError::bad_request(
                "cannot disable your own account",
            )));
        }
        state.auth.users().set_disabled(&user_id, disabled).await?;
        if disabled {
            state.auth.sessions().delete_for_user(&user_id).await?;
        }
    }

    if let Some(password) = request.password.as_deref() {
        state.auth.set_password(&user_id, password).await?;
        // A password reset invalidates every existing token of that account.
        state.auth.sessions().delete_for_user(&user_id).await?;
    }

    audit(
        &state,
        Some(&actor),
        "admin.user.update",
        Some(target.username.clone()),
        Some(format!(
            "role={:?} disabled={:?} password_reset={}",
            request.role,
            request.disabled,
            request.password.is_some()
        )),
    )
    .await;

    let row = repo(&state)
        .find_user(&user_id.0)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::NotFound(user_id.0.clone())))?;
    Ok(Json(row))
}

#[derive(Debug, Deserialize, Default)]
pub struct DeleteUserQuery {
    /// Purge `/private/<username>` as well (default: true).
    pub purge: Option<bool>,
}

pub async fn delete_user(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Path(id): Path<String>,
    Query(query): Query<DeleteUserQuery>,
) -> Result<StatusCode, ApiError> {
    let user_id = UserId::new(id);
    if actor.id == user_id {
        return Err(ApiError(MeiSyncError::bad_request(
            "cannot delete your own account",
        )));
    }
    let target = state
        .auth
        .users()
        .find_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::NotFound(user_id.0.clone())))?;
    let flags = state.auth.flags(&user_id).await?;
    if flags.role == "admin" && !flags.disabled && state.auth.users().count_admins().await? <= 1 {
        return Err(ApiError(MeiSyncError::bad_request(
            "cannot delete the last administrator",
        )));
    }
    let purge = query.purge.unwrap_or(true);
    if purge {
        state
            .files
            .admin_delete_namespace(&target.username)
            .await
            .map_err(ApiError)?;
    }
    repo(&state).devices().delete_for_user(&user_id).await?;
    state.auth.sessions().delete_for_user(&user_id).await?;
    state.auth.users().delete(&user_id).await?;
    audit(
        &state,
        Some(&actor),
        if purge {
            "admin.user.delete"
        } else {
            "admin.user.delete.keep_files"
        },
        Some(target.username.clone()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct LimitQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SessionList {
    pub sessions: Vec<AdminSessionRow>,
}

pub async fn list_sessions(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<LimitQuery>,
) -> Result<Json<SessionList>, ApiError> {
    let sessions = repo(&state)
        .list_sessions(query.limit.unwrap_or(200))
        .await?;
    Ok(Json(SessionList { sessions }))
}

pub async fn revoke_session(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // The console only knows the opaque session id, so the token is resolved here and never
    // travels through a URL, a log or the browser.
    let token = repo(&state)
        .session_token(&id)
        .await?
        .ok_or_else(|| ApiError(MeiSyncError::NotFound(format!("unknown session {id}"))))?;
    state.auth.logout(&token).await?;
    audit(
        &state,
        Some(&actor),
        "admin.session.revoke",
        Some(mask_token(&token)),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DeviceList {
    pub devices: Vec<AdminDeviceRow>,
}

pub async fn list_devices(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<LimitQuery>,
) -> Result<Json<DeviceList>, ApiError> {
    let devices = repo(&state)
        .list_devices(query.limit.unwrap_or(200))
        .await?;
    Ok(Json(DeviceList { devices }))
}

pub async fn delete_device(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Path((user_id, device_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    repo(&state)
        .delete_device(&UserId::new(user_id.clone()), &device_id)
        .await?;
    audit(
        &state,
        Some(&actor),
        "admin.device.delete",
        Some(format!("{user_id}/{device_id}")),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AuditList {
    pub entries: Vec<meisync_persistence::AuditRecord>,
}

pub async fn list_audit(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<LimitQuery>,
) -> Result<Json<AuditList>, ApiError> {
    let entries = repo(&state).list_audit(query.limit.unwrap_or(200)).await?;
    Ok(Json(AuditList { entries }))
}

// ---------------------------------------------------------------------------------------------
// File browser
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct FilesQuery {
    pub path: Option<String>,
    pub depth: Option<String>,
    /// Username or user id filter.
    pub owner: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileList {
    pub path: String,
    pub files: Vec<FileInfo>,
    /// `owner_id -> username`, so the console can show names without a second request.
    pub owners: std::collections::HashMap<String, String>,
}

pub async fn list_files(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<FilesQuery>,
) -> Result<Json<FileList>, ApiError> {
    let path = normalize_path(query.path.as_deref().unwrap_or("/"));
    let depth = Depth::parse(query.depth.as_deref().unwrap_or("1"));
    let owner_filter = match query.owner {
        Some(owner) if !owner.trim().is_empty() => Some(
            state
                .auth
                .users()
                .find_by_username(owner.trim())
                .await?
                .map(|u| u.id.0)
                .unwrap_or(owner),
        ),
        _ => None,
    };
    let entries = state.files.admin_list(&path, depth).await?;
    let owners: std::collections::HashMap<String, String> = state
        .auth
        .users()
        .list()
        .await?
        .into_iter()
        .map(|user| (user.id.0, user.username))
        .collect();
    let mut files = Vec::with_capacity(entries.len());
    for meta in entries {
        if let Some(owner) = &owner_filter {
            if &meta.owner_id.0 != owner {
                continue;
            }
        }
        let meta_path = state.files.path_of(&meta.id).await?;
        files.push(FileInfo::from_metadata(&meta, meta_path));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(FileList {
        path,
        files,
        owners,
    }))
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: String,
}

pub async fn get_file(
    State(state): State<AppState>,
    AdminUser(_user): AdminUser,
    Query(query): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let (meta, snapshot) = state
        .files
        .admin_read_content(&normalize_path(&query.path))
        .await?;
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));
    let mut response = Response::new(Body::from(snapshot.content));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&snapshot.etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "inline; filename=\"{}\"",
            meta.name.replace('"', "")
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("inline")),
    );
    Ok(response)
}

pub async fn delete_file(
    State(state): State<AppState>,
    AdminUser(actor): AdminUser,
    Query(query): Query<FileQuery>,
) -> Result<StatusCode, ApiError> {
    let path = normalize_path(&query.path);
    state.files.admin_delete_path(&path).await?;
    audit(&state, Some(&actor), "admin.file.delete", Some(path), None).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------------------------

/// Ensure the configured administrator exists and has the `admin` role. Called once at startup.
pub async fn bootstrap(state: &Arc<AppState>) -> anyhow::Result<()> {
    let Some(username) = state.config.admin_user.clone() else {
        return Ok(());
    };
    let existing = state.auth.users().find_by_username(&username).await?;
    let user = match existing {
        Some(user) => user,
        None => {
            let Some(password) = state.config.admin_password.clone() else {
                tracing::warn!(
                    user = %username,
                    "MEISYNC_ADMIN_USER is set but the account does not exist and no \
                     MEISYNC_ADMIN_PASSWORD was provided"
                );
                return Ok(());
            };
            let token = state.auth.register(&username, &password).await?;
            state
                .files
                .ensure_user_namespace(&token.user.id, &token.user.username)
                .await?;
            // The bootstrap token is not needed; the administrator logs in through the console.
            let _ = state.auth.logout(&token.token).await;
            state
                .auth
                .users()
                .find_by_id(&token.user.id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("bootstrapped administrator disappeared"))?
        }
    };
    state
        .files
        .ensure_user_namespace(&user.id, &user.username)
        .await?;
    let flags = state.auth.flags(&user.id).await?;
    let changed = flags.role != "admin" || flags.disabled;
    state.auth.users().set_role(&user.id, "admin").await?;
    state.auth.users().set_disabled(&user.id, false).await?;
    if changed {
        audit(
            state,
            Some(&user),
            "admin.bootstrap",
            Some(user.username.clone()),
            Some("configured administrator promoted/enabled".into()),
        )
        .await;
    }
    tracing::info!(user = %user.username, "administrator account is ready");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn normalize_path(path: &str) -> String {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

/// Mask a bearer token for display (console rows and audit entries).
pub(crate) fn mask_token(token: &str) -> String {
    if token.len() <= 10 {
        token.to_string()
    } else {
        format!("{}…{}", &token[..6], &token[token.len() - 4..])
    }
}

/// Small helper shared by the console frontend: the server advertises its admin API through
/// `GET /admin/api-info`, which keeps the static UI free of hard-coded paths.
pub async fn api_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "MeiSync Admin",
        "api": "/api/admin",
        "login": "/api/admin/login",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
