//! Obsidian / desktop sync API.
//!
//! This is deliberately a thin layer over [`FileService`]: the vault is a normal collection in the
//! user's private namespace, every file is still one YDoc, and conflicts are detected with the
//! same ETags WebDAV and S3 use. On top of that it adds three conveniences a file watcher needs:
//!
//! * a recursive manifest with relative paths and a revision hash,
//! * a WebSocket change feed fed by [`ChangeHub`](crate::events::ChangeHub),
//! * device registration so the admin console can see connected vaults.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use meisync_core::{MeiSyncError, Preconditions, User};
use meisync_persistence::SyncDevice;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ApiError, FileInfo};
use crate::extract::AuthUser;
use crate::service::{Depth, FileService};
use crate::state::AppState;

/// Protocol version advertised by the handshake. Bumped when a breaking change is made.
pub const SYNC_PROTOCOL_VERSION: u32 = 1;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/info", get(info))
        .route("/handshake", post(handshake))
        .route("/manifest", get(manifest))
        .route("/file", get(get_file).put(put_file).delete(delete_file))
        .route("/move", post(move_file))
        .route("/mkdir", post(mkdir))
        .route("/devices", get(list_devices).post(register_device))
        .route("/ws", get(websocket))
}

// ---------------------------------------------------------------------------------------------
// Root / path handling
// ---------------------------------------------------------------------------------------------

/// Normalise the client supplied vault root and anchor it under the user's private home.
///
/// `vault` is always interpreted relative to `/private/<username>`: a leading `/` is stripped and
/// the remaining segments are rebased under the caller's own namespace, so `vault=/private/alice`
/// becomes `/private/<caller>/private/alice` instead of addressing somebody else's home. `..`,
/// backslashes and NUL bytes are rejected. That keeps a stolen token unable to address another
/// user's namespace even if a future caller forgets a permission check.
pub fn resolve_vault_root(user: &User, vault: &str) -> Result<String, MeiSyncError> {
    let trimmed = vault.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err(MeiSyncError::bad_request(
            "vault is required (for example \"obsidian/MyVault\")",
        ));
    }
    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        let segment = segment.trim();
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains('\0') || segment.contains('\\') {
            return Err(MeiSyncError::bad_request("invalid vault path"));
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(MeiSyncError::bad_request("invalid vault path"));
    }
    Ok(format!("/private/{}/{}", user.username, segments.join("/")))
}

fn relative(root: &str, absolute: &str) -> Option<String> {
    if absolute == root {
        return Some(String::new());
    }
    absolute
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(|rest| rest.to_string())
}

fn absolute_for(root: &str, path: &str) -> Result<String, MeiSyncError> {
    let path = path.trim().trim_matches('/');
    if path.is_empty() {
        return Err(MeiSyncError::bad_request("path is required"));
    }
    for segment in path.split('/') {
        if segment == ".." || segment == "." || segment.contains('\0') || segment.contains('\\') {
            return Err(MeiSyncError::bad_request("invalid path"));
        }
    }
    Ok(format!("{}/{}", root.trim_end_matches('/'), path))
}

async fn ensure_root(state: &AppState, user: &User, root: &str) -> Result<(), ApiError> {
    let meta = state
        .files
        .ensure_collection_path(user, root)
        .await
        .map_err(ApiError)?;
    if !meta.is_collection() {
        return Err(ApiError(MeiSyncError::Conflict(format!(
            "{root} exists but is not a collection"
        ))));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Handshake / info
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SyncInfo {
    pub service: &'static str,
    pub version: &'static str,
    pub protocol: u32,
    pub capabilities: Vec<&'static str>,
    pub max_body_bytes: usize,
    pub conflict_model: &'static str,
}

pub async fn info(State(state): State<AppState>) -> Json<SyncInfo> {
    Json(SyncInfo {
        service: "meisync",
        version: env!("CARGO_PKG_VERSION"),
        protocol: SYNC_PROTOCOL_VERSION,
        capabilities: vec![
            "manifest",
            "content",
            "etag-conflict",
            "move",
            "mkdir",
            "websocket",
            "devices",
            "binary",
        ],
        max_body_bytes: state.config.max_body_bytes,
        conflict_model: "etag",
    })
}

#[derive(Debug, Deserialize)]
pub struct HandshakeRequest {
    pub vault: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub plugin_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HandshakeResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub protocol: u32,
    pub user: meisync_core::PublicUser,
    /// Relative vault root as supplied by the client (`obsidian/MyVault`).
    pub vault: String,
    /// Absolute namespace path of the vault (`/private/alice/obsidian/MyVault`).
    pub root: String,
    /// Absolute path of the account home (`/private/alice`).
    pub home: String,
    pub server_time: chrono::DateTime<Utc>,
    pub seq: u64,
    pub max_body_bytes: usize,
    pub capabilities: Vec<&'static str>,
}

pub async fn handshake(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(request): Json<HandshakeRequest>,
) -> Result<Json<HandshakeResponse>, ApiError> {
    let root = resolve_vault_root(&user, &request.vault)?;
    ensure_root(&state, &user, &root).await?;
    if let Some(device_id) = request.device_id.as_deref().filter(|id| !id.is_empty()) {
        let now = Utc::now();
        let device = SyncDevice {
            user_id: user.id.clone(),
            device_id: device_id.to_string(),
            vault: request.vault.trim_matches('/').to_string(),
            name: request.name.unwrap_or_else(|| "Obsidian".into()),
            platform: request.platform.unwrap_or_default(),
            plugin_version: request.plugin_version.unwrap_or_default(),
            last_seen_at: now,
            created_at: now,
        };
        meisync_persistence::SyncDeviceRepository::new(state.storage.db.clone())
            .upsert(&device)
            .await?;
    }
    Ok(Json(HandshakeResponse {
        service: "meisync",
        version: env!("CARGO_PKG_VERSION"),
        protocol: SYNC_PROTOCOL_VERSION,
        user: user.to_public(),
        vault: request.vault.trim_matches('/').to_string(),
        root,
        home: format!("/private/{}", user.username),
        server_time: Utc::now(),
        seq: state.changes.last_seq(),
        max_body_bytes: state.config.max_body_bytes,
        capabilities: vec![
            "manifest",
            "content",
            "etag-conflict",
            "move",
            "mkdir",
            "websocket",
            "devices",
            "binary",
        ],
    }))
}

// ---------------------------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SyncEntry {
    /// Path relative to the vault root, without a leading slash.
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub content_type: String,
    pub version: i64,
    pub sync_mode: String,
    pub updated_at: chrono::DateTime<Utc>,
    pub file_id: String,
}

#[derive(Debug, Serialize)]
pub struct ManifestResponse {
    pub root: String,
    pub vault: String,
    pub revision: String,
    pub generated_at: chrono::DateTime<Utc>,
    pub seq: u64,
    pub entries: Vec<SyncEntry>,
}

#[derive(Debug, Deserialize)]
pub struct VaultQuery {
    pub vault: String,
}

pub async fn manifest(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<VaultQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let root = resolve_vault_root(&user, &query.vault)?;
    ensure_root(&state, &user, &root).await?;
    let mut entries = Vec::new();
    let mut hasher = Sha256::new();
    for meta in state.files.list(&user, &root, Depth::Infinite).await? {
        let absolute = state.files.path_of(&meta.id).await?;
        let Some(path) = relative(&root, &absolute) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let entry = SyncEntry {
            path: path.clone(),
            name: meta.name.clone(),
            kind: meta.kind.as_str().to_string(),
            size: meta.size,
            etag: meta.etag.clone(),
            content_type: meta
                .content_type
                .clone()
                .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name)),
            version: meta.version,
            sync_mode: meta.sync_mode.as_str().to_string(),
            updated_at: meta.updated_at,
            file_id: meta.id.0.clone(),
        };
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.etag.as_deref().unwrap_or(""));
        hasher.update([0]);
        hasher.update(entry.version.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(entry.kind.as_bytes());
        hasher.update(b"\n");
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let revision = hex::encode(hasher.finalize());
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        if value.trim_matches('"') == revision {
            let mut response = StatusCode::NOT_MODIFIED.into_response();
            if let Ok(etag) = HeaderValue::from_str(&format!("\"{revision}\"")) {
                response.headers_mut().insert(header::ETAG, etag);
            }
            return Ok(response);
        }
    }
    let body = ManifestResponse {
        root,
        vault: query.vault.trim_matches('/').to_string(),
        revision: revision.clone(),
        generated_at: Utc::now(),
        seq: state.changes.last_seq(),
        entries,
    };
    let mut response = Json(body).into_response();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{revision}\"")) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    Ok(response)
}

// ---------------------------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub vault: String,
    pub path: String,
}

pub async fn get_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let root = resolve_vault_root(&user, &query.vault)?;
    let absolute = absolute_for(&root, &query.path)?;
    let meta = state.files.get(&user, &absolute).await?;
    if meta.is_collection() {
        return Err(ApiError(MeiSyncError::Conflict(
            "path is a collection".into(),
        )));
    }
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        if meta
            .etag
            .as_deref()
            .map(|etag| meisync_core::normalize_etag(value) == meisync_core::normalize_etag(etag))
            .unwrap_or(false)
        {
            let mut response = StatusCode::NOT_MODIFIED.into_response();
            if let Ok(etag) = HeaderValue::from_str(meta.etag.as_deref().unwrap_or("")) {
                response.headers_mut().insert(header::ETAG, etag);
            }
            return Ok(response);
        }
    }
    let snapshot = state.files.read_content(&user, &absolute).await?;
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));
    let mut response = Response::new(axum::body::Body::from(snapshot.content));
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&snapshot.etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

#[derive(Debug, Serialize)]
pub struct WriteResponse {
    pub path: String,
    pub etag: String,
    pub size: u64,
    pub version: i64,
    pub sync_mode: String,
    pub created: bool,
}

pub async fn put_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<WriteResponse>), ApiError> {
    let root = resolve_vault_root(&user, &query.vault)?;
    let absolute = absolute_for(&root, &query.path)?;
    let existed = state.files.resolve_path(&absolute).await.is_ok();
    let preconditions = Preconditions::new(
        headers
            .get(header::IF_MATCH)
            .and_then(|value| value.to_str().ok()),
        headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok()),
    );
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    let snapshot = state
        .files
        .replace_content_checked(&user, &absolute, body.to_vec(), content_type, preconditions)
        .await?;
    Ok((
        if existed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(WriteResponse {
            path: query.path.trim_matches('/').to_string(),
            etag: snapshot.etag,
            size: snapshot.size,
            version: snapshot.version,
            sync_mode: snapshot.sync_mode.as_str().to_string(),
            created: !existed,
        }),
    ))
}

pub async fn delete_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let root = resolve_vault_root(&user, &query.vault)?;
    let absolute = absolute_for(&root, &query.path)?;
    let preconditions = Preconditions::new(
        headers
            .get(header::IF_MATCH)
            .and_then(|value| value.to_str().ok()),
        headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok()),
    );
    // The precondition is evaluated by the service, inside the document lock (§25): checking it
    // here against a separately read metadata row would let a concurrent write slip in between.
    state.files.delete_checked(&user, &absolute, preconditions).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Move / mkdir
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MoveRequest {
    pub vault: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub overwrite: bool,
}

pub async fn move_file(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(request): Json<MoveRequest>,
) -> Result<Json<FileInfo>, ApiError> {
    let root = resolve_vault_root(&user, &request.vault)?;
    let from = absolute_for(&root, &request.from)?;
    let to = absolute_for(&root, &request.to)?;
    let (to_parent, to_name) = crate::service::DefaultFileService::split_parent(&to)?;
    let meta = state
        .files
        .move_file(&user, &from, &to_parent, Some(&to_name), request.overwrite)
        .await?;
    let path = state.files.path_of(&meta.id).await?;
    Ok(Json(FileInfo::from_metadata(&meta, path)))
}

#[derive(Debug, Deserialize)]
pub struct MkdirRequest {
    pub vault: String,
    pub path: String,
}

pub async fn mkdir(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(request): Json<MkdirRequest>,
) -> Result<Json<FileInfo>, ApiError> {
    let root = resolve_vault_root(&user, &request.vault)?;
    let absolute = absolute_for(&root, &request.path)?;
    let meta = state.files.ensure_collection_path(&user, &absolute).await?;
    if !meta.is_collection() {
        return Err(ApiError(MeiSyncError::Conflict(format!(
            "{} exists but is not a collection",
            request.path
        ))));
    }
    let path = state.files.path_of(&meta.id).await?;
    Ok(Json(FileInfo::from_metadata(&meta, path)))
}

// ---------------------------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DeviceRequest {
    pub vault: String,
    pub device_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub plugin_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeviceResponse {
    pub device: SyncDeviceView,
    pub server_time: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct SyncDeviceView {
    pub device_id: String,
    pub vault: String,
    pub name: String,
    pub platform: String,
    pub plugin_version: String,
    pub last_seen_at: chrono::DateTime<Utc>,
}

impl From<&SyncDevice> for SyncDeviceView {
    fn from(value: &SyncDevice) -> Self {
        Self {
            device_id: value.device_id.clone(),
            vault: value.vault.clone(),
            name: value.name.clone(),
            platform: value.platform.clone(),
            plugin_version: value.plugin_version.clone(),
            last_seen_at: value.last_seen_at,
        }
    }
}

pub async fn register_device(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(request): Json<DeviceRequest>,
) -> Result<Json<DeviceResponse>, ApiError> {
    let root = resolve_vault_root(&user, &request.vault)?;
    ensure_root(&state, &user, &root).await?;
    let now = Utc::now();
    let device = SyncDevice {
        user_id: user.id.clone(),
        device_id: request.device_id.clone(),
        vault: request.vault.trim_matches('/').to_string(),
        name: request.name.unwrap_or_else(|| "Obsidian".into()),
        platform: request.platform.unwrap_or_default(),
        plugin_version: request.plugin_version.unwrap_or_default(),
        last_seen_at: now,
        created_at: now,
    };
    meisync_persistence::SyncDeviceRepository::new(state.storage.db.clone())
        .upsert(&device)
        .await?;
    if let Err(err) = meisync_persistence::AuditRepository::new(state.storage.db.clone())
        .record(
            Some(user.id.0.clone()),
            Some(user.username.clone()),
            "sync.device",
            Some(format!("{}/{}", request.vault, request.device_id)),
            Some(format!("{} {}", device.platform, device.plugin_version)),
        )
        .await
    {
        tracing::warn!(error = %err, "failed to write device audit entry");
    }
    Ok(Json(DeviceResponse {
        device: SyncDeviceView::from(&device),
        server_time: now,
    }))
}

#[derive(Debug, Serialize)]
pub struct DeviceList {
    pub devices: Vec<SyncDeviceView>,
}

pub async fn list_devices(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<DeviceList>, ApiError> {
    let devices = meisync_persistence::SyncDeviceRepository::new(state.storage.db.clone())
        .list_for_user(&user.id)
        .await?;
    Ok(Json(DeviceList {
        devices: devices.iter().map(SyncDeviceView::from).collect(),
    }))
}

// ---------------------------------------------------------------------------------------------
// WebSocket change feed
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub vault: String,
}

#[derive(Debug, Serialize)]
struct WsReady {
    r#type: &'static str,
    protocol: u32,
    root: String,
    seq: u64,
}

#[derive(Debug, Serialize)]
struct WsEvent<'a> {
    r#type: &'static str,
    change: &'a crate::events::FileChange,
}

#[derive(Debug, Serialize)]
struct WsControl {
    r#type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

pub async fn websocket(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let root = resolve_vault_root(&user, &query.vault)?;
    ensure_root(&state, &user, &root).await?;
    let mut changes = state.changes.subscribe();
    Ok(upgrade.on_upgrade(move |socket| async move {
        if let Err(err) = run_socket(socket, root, state.changes.last_seq(), &mut changes).await {
            tracing::debug!(error = %err, "sync websocket closed");
        }
    }))
}

async fn run_socket(
    socket: WebSocket,
    root: String,
    seq: u64,
    changes: &mut tokio::sync::broadcast::Receiver<crate::events::FileChange>,
) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let ready = serde_json::to_string(&WsReady {
        r#type: "ready",
        protocol: SYNC_PROTOCOL_VERSION,
        root: root.clone(),
        seq,
    })?;
    sender.send(Message::Text(ready.into())).await?;

    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` yields immediately on its first tick; consume it so the first frame the client
    // receives is always the JSON `ready` message.
    ticker.tick().await;
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                        if value.get("type").and_then(|v| v.as_str()) == Some("ping") {
                            let pong = serde_json::json!({"type": "pong", "seq": seq});
                            sender.send(Message::Text(pong.to_string().into())).await?;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        sender.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            change = changes.recv() => {
                match change {
                    Ok(change) if change.touches(&root) => {
                        let payload = serde_json::to_string(&WsEvent { r#type: "change", change: &change })?;
                        sender.send(Message::Text(payload.into())).await?;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let payload = serde_json::to_string(&WsControl { r#type: "resync", reason: Some("lagged") })?;
                        sender.send(Message::Text(payload.into())).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = ticker.tick() => {
                sender.send(Message::Ping(Vec::new().into())).await?;
            }
        }
    }
    Ok(())
}
