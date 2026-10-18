//! YSync: realtime CRDT synchronisation (`docs/mvp.md` §9, §10, §34).
//!
//! ```text
//! WebSocket
//!   ↓ authenticate  (bearer token / ?token= / subprotocol)
//!   ↓ authorize     (PermissionService through FileService)
//!   ↓ join room     (one room per FileDocument)
//!   ↓ Yjs/Yrs sync protocol
//! ```
//!
//! The wire format is the standard `y-websocket` framing implemented by
//! `yrs::sync::protocol`:
//!
//! ```text
//! [messageType varuint] [message]
//!   0 sync      → [syncType varuint] 0 step1 | 1 step2 | 2 update
//!   1 awareness → [len varuint] [awareness payload]
//!   2 auth      → 0 denied + reason | 1 granted
//!   3 queryAwareness
//! ```
//!
//! Two application level extensions sit on top of it:
//!
//! * `auth denied` with reason `deleted` tells a client that its file is gone (§28).
//! * `custom(42, path)` announces a rename / move of the document (§29). YSync binds to the
//!   `FileId`, so clients may safely ignore it.
//!
//! Both directions are handled here: document updates are applied through the
//! [`meisync_document::DocumentHandle`] (persistence + broadcast), never by touching the YDoc
//! behind the manager's back.

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::response::{IntoResponse, Response};
use meisync_api::ApiError;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use meisync_api::service::FileService;
use meisync_api::{AppState, ChangeKind, FileChange};
use meisync_core::{FileMetadata, MeiSyncError, User};
use meisync_document::{DocumentHandle, Room, RoomEvent};
use std::sync::Arc;
use tokio::sync::mpsc;
use yrs::encoding::read::Cursor;
use yrs::sync::awareness::AwarenessUpdate;
use yrs::sync::protocol::{Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::Update;

pub const YSYNC_MOUNT: &str = "/ysync";
/// Tag used for the "document moved" notification.
///
/// Must stay below 128: `yrs` writes custom tags as a raw `u8` but reads them back as a
/// lib0 var-uint, so anything >= 0x80 would corrupt the frame.
pub const MSG_DOC_MOVED: u8 = 42;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ysync", get(upgrade_root))
        .route("/ysync/", get(upgrade_root))
        .route("/ysync/{*path}", get(upgrade))
}

/// Encode one complete protocol frame (including the message type prefix).
pub fn encode_message(message: &Message) -> Vec<u8> {
    let mut encoder = EncoderV1::new();
    message.encode(&mut encoder);
    encoder.to_vec()
}

/// Encode several frames into one binary WebSocket message.
pub fn encode_messages(messages: &[Message]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        out.extend_from_slice(&encode_message(message));
    }
    out
}

/// Decode every protocol frame contained in one binary WebSocket message.
pub fn decode_frames(data: &[u8]) -> Vec<Message> {
    let mut decoder = DecoderV1::new(Cursor::new(data));
    let reader = MessageReader::new(&mut decoder);
    let mut out = Vec::new();
    for result in reader {
        match result {
            Ok(message) => out.push(message),
            Err(_) => break,
        }
    }
    out
}

/// Namespace path of a YSync URL: `/ysync/private/alice/todo.md` → `/private/alice/todo.md`.
pub fn namespace_path(uri_path: &str, mount: &str) -> String {
    let mut path = uri_path.to_string();
    let mount = mount.trim_end_matches('/');
    if !mount.is_empty() {
        if let Some(rest) = path.strip_prefix(mount) {
            path = rest.to_string();
        }
    }
    let decoded = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect::<Vec<String>>()
        .join("/");
    format!("/{decoded}")
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

async fn upgrade_root(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, meisync_api::ApiError> {
    let path = namespace_path(request.uri().path(), YSYNC_MOUNT);
    upgrade_inner(state, request, &path).await
}

async fn upgrade(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, meisync_api::ApiError> {
    let path = namespace_path(request.uri().path(), YSYNC_MOUNT);
    upgrade_inner(state, request, &path).await
}

/// Authenticate → authorize → only then require a WebSocket upgrade, so that an
/// unauthenticated request answers `401` instead of leaking `400 Bad Request` from the
/// extractor.
async fn upgrade_inner(
    state: AppState,
    request: Request,
    path: &str,
) -> Result<Response, meisync_api::ApiError> {
    let (mut parts, _) = request.into_parts();

    // Authentication: WebSocket clients cannot always set headers, so `?token=` is supported.
    let token = meisync_api::extract::AuthToken::from_parts(&parts)
        .or_else(|| meisync_api::extract::AuthToken::from_query(&parts))
        .or_else(|| protocol_token(&parts.headers))
        .ok_or_else(|| {
            ApiError(MeiSyncError::Unauthorized("YSync requires a token".into()))
        })?;
    let user = state
        .auth
        .authenticate(&token)
        .await
        .map_err(ApiError)?;

    // Authorization: the FileService resolves the document *and* checks the sync permission.
    let (meta, handle) = state
        .files
        .open_document(&user, path)
        .await
        .map_err(ApiError)?;
    // `SYNC` is the read gate of the room; writing an update additionally needs `WRITE`
    // (owner or an explicit ACL grant) – a public document is read+sync only (§12).
    let can_write = state
        .files
        .effective_permissions(&user, &meta)
        .await
        .map_err(ApiError)?
        .has(meisync_core::PermissionSet::WRITE);

    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        // Authenticated, but not a WebSocket handshake (a plain GET, for instance).
        Err(rejection) => return Ok(rejection.into_response()),
    };

    Ok(upgrade.on_upgrade(move |socket| {
        let state = Arc::new(state);
        async move {
            if let Err(err) = session(state, user, meta, handle, can_write, socket).await {
                tracing::debug!(error = %err, "ysync session ended");
            }
        }
    }))
}

fn protocol_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;
    for part in value.split(',') {
        let part = part.trim();
        if let Some(token) = part.strip_prefix("bearer.") {
            return Some(token.to_string());
        }
    }
    None
}

/// The per-connection sync session.
async fn session(
    state: Arc<AppState>,
    user: User,
    meta: FileMetadata,
    handle: DocumentHandle,
    can_write: bool,
    socket: WebSocket,
) -> Result<(), MeiSyncError> {
    let file_id = meta.id.clone();
    let room = state.documents.room(&file_id).await;
    let (conn, mut events) = room.subscribe();
    let (mut sink, mut stream) = socket.split();
    let (reply_tx, mut replies) = mpsc::unbounded_channel::<Vec<u8>>();

    // Handshake: our state vector + the awareness currently known for the room (§9, §10).
    let mut initial: Vec<Message> = vec![Message::Sync(SyncMessage::SyncStep1(
        handle.state_vector().await,
    ))];
    if let Some(update) = room
        .with_awareness(|awareness| awareness.update().ok())
        .flatten()
    {
        initial.push(Message::Awareness(update));
    }
    let _ = sink
        .send(WsMessage::Binary(encode_messages(&initial).into()))
        .await;

    // Writer task: room events and protocol replies are complete frames already.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                frame = replies.recv() => match frame {
                    Some(frame) => {
                        if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                event = events.recv() => match event {
                    Some(event) => match room_event_to_frame(event) {
                        Some(frame) => {
                            if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    None => break,
                },
            }
        }
    });

    let result = read_loop(
        &state,
        &user,
        &meta,
        &handle,
        &room,
        conn,
        can_write,
        &mut stream,
        &reply_tx,
    )
    .await;

    // Leaving the room also drops the awareness states this connection introduced (§10).
    state.documents.unsubscribe(&file_id, conn).await;
    writer.abort();
    result
}

fn room_event_to_frame(event: RoomEvent) -> Option<Vec<u8>> {
    match event {
        RoomEvent::Update(bytes) => {
            Some(encode_message(&Message::Sync(SyncMessage::Update(bytes))))
        }
        RoomEvent::Awareness(payload) => {
            let update = AwarenessUpdate::decode_v1(&payload).ok()?;
            Some(encode_message(&Message::Awareness(update)))
        }
        RoomEvent::Deleted => Some(encode_message(&Message::Auth(Some(
            "deleted".to_string(),
        )))),
        RoomEvent::Moved { path } => Some(encode_message(&Message::Custom(
            MSG_DOC_MOVED,
            path.into_bytes(),
        ))),
    }
}

async fn read_loop(
    state: &Arc<AppState>,
    user: &User,
    meta: &FileMetadata,
    handle: &DocumentHandle,
    room: &Arc<Room>,
    conn: meisync_document::ConnId,
    can_write: bool,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    replies: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), MeiSyncError> {
    while let Some(Ok(message)) = stream.next().await {
        let payload = match message {
            WsMessage::Binary(bytes) => bytes.to_vec(),
            WsMessage::Text(text) => text.as_bytes().to_vec(),
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
        };

        for frame in decode_frames(&payload) {
            match frame {
                // Client asks for everything it is missing → answer with the YDoc diff.
                Message::Sync(SyncMessage::SyncStep1(state_vector)) => {
                    let diff = handle.diff(&state_vector).await;
                    let _ = replies.send(encode_message(&Message::Sync(SyncMessage::SyncStep2(diff))));
                }
                // Either the answer to our own step 1 (full state of a fresh client) or a
                // continuous update. Both go through the DocumentManager.
                Message::Sync(SyncMessage::SyncStep2(bytes))
                | Message::Sync(SyncMessage::Update(bytes)) => {
                    if !can_write {
                        // A client that is already up to date answers our `SyncStep1` with an
                        // empty diff; that is not a write. Everything that carries content
                        // needs `WRITE` (owner or ACL, §12).
                        let empty = Update::decode_v1(&bytes)
                            .map(|update| update.is_empty())
                            .unwrap_or(false);
                        if !empty {
                            tracing::warn!(
                                user = %user.username,
                                "refused ysync update without write access"
                            );
                            let _ = replies.send(encode_message(&Message::Auth(Some(
                                "write access denied".to_string(),
                            ))));
                        }
                        continue;
                    }
                    // An empty diff is the normal answer of an already up-to-date client to our
                    // own `SyncStep1`; it carries no content and must not count as a write.
                    let non_empty = Update::decode_v1(&bytes)
                        .map(|update| !update.is_empty())
                        .unwrap_or(false);
                    if !non_empty {
                        continue;
                    }
                    match handle.apply_update(&user.id, bytes, Some(conn)).await {
                        Ok(snapshot) => {
                            announce_ysync_update(state, user, meta, &snapshot).await;
                        }
                        Err(err) => tracing::warn!(error = %err, "failed to apply ysync update"),
                    }
                }
                Message::Awareness(update) => {
                    for client in update.clients.keys().copied() {
                        room.note_client(conn, client);
                    }
                    let payload = update.encode_v1();
                    room.with_awareness(|awareness| {
                        let _ = awareness.apply_update(update);
                    });
                    room.broadcast(RoomEvent::Awareness(payload), Some(conn));
                }
                Message::AwarenessQuery => {
                    if let Some(update) = room
                        .with_awareness(|awareness| awareness.update().ok())
                        .flatten()
                    {
                        let _ = replies.send(encode_message(&Message::Awareness(update)));
                    }
                }
                Message::Auth(None) => {}
                Message::Auth(Some(reason)) => {
                    return Err(MeiSyncError::Forbidden(reason));
                }
                Message::Custom(tag, data) => {
                    tracing::debug!(tag, len = data.len(), "ignoring custom ysync message");
                }
            }
        }
    }
    Ok(())
}

/// Publish a file-level change for a CRDT edit.
///
/// The sync feed is what lets *closed* copies of a note (another Obsidian vault, a WebDAV/S3
/// mirror, an admin view) notice an edit that only ever touched the YDoc. Without this, those
/// clients had to wait for their next manifest poll because YSync writes bypass
/// `FileService::replace_content` – the only other publisher on the hub.
///
/// The event is metadata only, exactly like the WebDAV/S3 ones: consumers re-read the manifest
/// and fetch the materialised bytes themselves. Publishing per update is cheap (the client
/// debounces rounds) and guarantees the last update of a burst is never lost to a timer.
async fn announce_ysync_update(
    state: &Arc<AppState>,
    user: &User,
    meta: &FileMetadata,
    snapshot: &meisync_document::DocumentSnapshot,
) {
    let path = match state.files.path_of(&meta.id).await {
        Ok(path) => path,
        Err(err) => {
            tracing::debug!(error = %err, "could not resolve the path of a ysync update");
            return;
        }
    };
    state.changes.publish(FileChange {
        seq: 0,
        kind: ChangeKind::Updated,
        file_id: meta.id.0.clone(),
        path,
        old_path: None,
        etag: Some(snapshot.etag.clone()),
        size: snapshot.size,
        collection: false,
        updated_at: chrono::Utc::now(),
        actor_id: user.id.0.clone(),
    });
}

/// WebSocket endpoint rejection with a proper status code.
pub fn rejected(err: &MeiSyncError) -> Response {
    let status = meisync_api::error::status_of(err);
    let mut response = Response::new(axum::body::Body::from(format!("{err}\n")));
    *response.status_mut() = status;
    response
}