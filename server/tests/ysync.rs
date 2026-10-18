//! End-to-end YSync coverage (`docs/mvp.md` §9, §10, §19, §28, §29).
//!
//! These tests drive real WebSocket connections against a real listener and speak the
//! `y-websocket` framing with two independent `yrs` clients, so the CRDT convergence is verified
//! through the real protocol stack – not through an in-process shortcut.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use tower::ServiceExt;
use meisync_ysync::{decode_frames, encode_messages};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use yrs::sync::protocol::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, ReadTxn, Text, Transact, Update, WriteTxn};

type Client = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

struct YSyncServer {
    addr: std::net::SocketAddr,
    _dir: tempfile::TempDir,
}

impl YSyncServer {
    async fn spawn() -> (YSyncServer, std::sync::Arc<meisync_api::AppState>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = meisync_api::ServerConfig {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let state = meisync_api::AppState::bootstrap(config)
            .await
            .expect("bootstrap");
        let app = meisync_server::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (YSyncServer { addr, _dir: dir }, state)
    }
}

async fn connect(server: &YSyncServer, token: &str, path: &str) -> Client {
    let url = format!(
        "ws://{}/ysync{}?token={}",
        server.addr, path, token
    );
    let (socket, _response) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio_tungstenite::connect_async(url).await
    })
    .await
    .expect("connect timeout")
    .expect("handshake");
    socket
}

async fn register(state: &std::sync::Arc<meisync_api::AppState>, username: &str) -> String {
    let user = state
        .auth
        .register(username, "secret123")
        .await
        .expect("register");
    state
        .files
        .ensure_user_namespace(&user.user.id, &user.user.username)
        .await
        .expect("namespace");
    user.token
}

async fn put(state: &std::sync::Arc<meisync_api::AppState>, token: &str, path: &str, body: &str) {
    use meisync_api::FileService;
    let user = state.auth.authenticate(token).await.expect("auth");
    state
        .files
        .replace_content(
            &user,
            path,
            body.as_bytes().to_vec(),
            Some("text/markdown".into()),
        )
        .await
        .expect("put");
}

async fn get(state: &std::sync::Arc<meisync_api::AppState>, token: &str, path: &str) -> String {
    use meisync_api::FileService;
    let user = state.auth.authenticate(token).await.expect("auth");
    let snapshot = state.files.read_content(&user, path).await.expect("get");
    String::from_utf8_lossy(&snapshot.content).to_string()
}

async fn send(socket: &mut Client, message: Message) {
    socket
        .send(WsMessage::Binary(encode_messages(&[message]).into()))
        .await
        .expect("send");
}

async fn recv(socket: &mut Client) -> Vec<Message> {
    let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("recv timeout")
        .expect("stream")
        .expect("ws frame");
    match frame {
        WsMessage::Binary(bytes) => decode_frames(&bytes),
        other => panic!("unexpected frame: {other:?}"),
    }
}

/// Receive the next frame, or `None` when nothing arrives within `wait`
/// (used by tests that assert that *something* does or does not arrive).
async fn try_recv(socket: &mut Client, wait: Duration) -> Option<Vec<Message>> {
    match tokio::time::timeout(wait, socket.next()).await {
        Ok(Some(Ok(WsMessage::Binary(bytes)))) => Some(decode_frames(&bytes)),
        _ => None,
    }
}

/// Perform the handshake: exchange step 1 / step 2 until both sides are in sync.
async fn handshake(socket: &mut Client, doc: &Doc) {
    let state_vector = doc.transact().state_vector();
    send(socket, Message::Sync(SyncMessage::SyncStep1(state_vector))).await;

    // The server answers with its own step 1 (plus awareness) and with our step 2.
    let mut applied = false;
    for _ in 0..4 {
        let frames = recv(socket).await;
        for frame in frames {
            match frame {
                Message::Sync(SyncMessage::SyncStep1(sv)) => {
                    let diff = doc.transact().encode_diff_v1(&sv);
                    send(socket, Message::Sync(SyncMessage::SyncStep2(diff))).await;
                }
                Message::Sync(SyncMessage::SyncStep2(bytes)) => {
                    let update = Update::decode_v1(&bytes).expect("update");
                    let mut txn = doc.transact_mut();
                    txn.apply_update(update).expect("apply");
                    applied = true;
                }
                _ => {}
            }
        }
        if applied {
            break;
        }
    }
    assert!(applied, "handshake did not deliver the document state");
}

fn content_of(doc: &Doc) -> String {
    let txn = doc.transact();
    match txn.get_text("content") {
        Some(text) => text.get_string(&txn),
        None => String::new(),
    }
}

fn edit(doc: &mut Doc, index: u32, chunk: &str) -> Vec<u8> {
    let mut txn = doc.transact_mut();
    let text = txn.get_or_insert_text("content");
    text.insert(&mut txn, index, chunk);
    txn.encode_update_v1()
}

#[tokio::test]
async fn two_clients_converge_through_the_sync_protocol() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/notes.md", "hello").await;

    let mut a = connect(&server, &token, "/private/alice/notes.md").await;
    let mut b = connect(&server, &token, "/private/alice/notes.md").await;

    let mut doc_a = Doc::new();
    let doc_b = Doc::new();
    handshake(&mut a, &doc_a).await;
    handshake(&mut b, &doc_b).await;
    assert_eq!(content_of(&doc_a), "hello");
    assert_eq!(content_of(&doc_b), "hello");

    // Client A appends text -> the update reaches B and the server.
    let update = edit(&mut doc_a, 5, " world");
    send(&mut a, Message::Sync(SyncMessage::Update(update))).await;

    let mut received = false;
    for _ in 0..8 {
        for frame in recv(&mut b).await {
            if let Message::Sync(SyncMessage::SyncStep2(bytes)) | Message::Sync(SyncMessage::Update(bytes)) = frame {
                let update = Update::decode_v1(&bytes).expect("update");
                let mut txn = doc_b.transact_mut();
                txn.apply_update(update).expect("apply");
            }
        }
        if content_of(&doc_b) == "hello world" {
            received = true;
            break;
        }
    }
    assert!(received, "client B did not receive the update");
    assert_eq!(content_of(&doc_b), "hello world");

    // WebDAV now materialises the very same YDoc state (§2.2, §27).
    assert_eq!(get(&state, &token, "/private/alice/notes.md").await, "hello world");

    // And a WebDAV write is pushed to the connected clients.
    put(&state, &token, "/private/alice/notes.md", "hello world!").await;
    let mut synced = false;
    for _ in 0..8 {
        for frame in recv(&mut a).await {
            if let Message::Sync(SyncMessage::SyncStep2(bytes)) | Message::Sync(SyncMessage::Update(bytes)) = frame {
                let update = Update::decode_v1(&bytes).expect("update");
                let mut txn = doc_a.transact_mut();
                txn.apply_update(update).expect("apply");
            }
        }
        if content_of(&doc_a) == "hello world!" {
            synced = true;
            break;
        }
    }
    assert!(synced, "client A did not see the WebDAV write");
    assert_eq!(content_of(&doc_a), "hello world!");
}

#[tokio::test]
async fn incremental_sync_only_transfers_the_difference() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/big.md", &"a".repeat(4096)).await;

    let mut a = connect(&server, &token, "/private/alice/big.md").await;
    let doc_a = Doc::new();
    handshake(&mut a, &doc_a).await;

    // A client that is already up to date receives a tiny (empty) diff.
    send(&mut a, Message::Sync(SyncMessage::SyncStep1(doc_a.transact().state_vector()))).await;
    let mut diff_len = None;
    for _ in 0..4 {
        for frame in recv(&mut a).await {
            if let Message::Sync(SyncMessage::SyncStep2(bytes)) = frame {
                diff_len = Some(bytes.len());
            }
        }
        if diff_len.is_some() {
            break;
        }
    }
    let diff_len = diff_len.expect("step2 response");
    assert!(diff_len < 64, "incremental diff was {diff_len} bytes");

    // A fresh client receives the full state.
    let mut b = connect(&server, &token, "/private/alice/big.md").await;
    let doc_b = Doc::new();
    handshake(&mut b, &doc_b).await;
    assert_eq!(content_of(&doc_b).len(), 4096);
}

#[tokio::test]
async fn awareness_is_relayed_and_dropped_on_disconnect() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/aw.md", "x").await;

    let mut a = connect(&server, &token, "/private/alice/aw.md").await;
    let mut b = connect(&server, &token, "/private/alice/aw.md").await;
    let doc_a = Doc::new();
    let doc_b = Doc::new();
    handshake(&mut a, &doc_a).await;
    handshake(&mut b, &doc_b).await;

    // Client A announces its cursor; client B must see it.
    let awareness = yrs::sync::awareness::AwarenessUpdate {
        clients: std::collections::HashMap::from([(
            yrs::block::ClientID::new(4242),
            yrs::sync::awareness::AwarenessUpdateEntry {
                clock: 1,
                json: "{\"cursor\":100}".into(),
            },
        )]),
    };
    send(&mut a, Message::Awareness(awareness)).await;

    let mut seen = false;
    for _ in 0..8 {
        let frames = recv(&mut b).await;
        assert!(!frames.is_empty(), "empty protocol frame");
        for frame in frames {
            if let Message::Awareness(update) = frame {
                let entry = update.clients.get(&yrs::block::ClientID::new(4242));
                assert_eq!(
                    entry.map(|e| e.json.to_string()),
                    Some("{\"cursor\":100}".to_string())
                );
                seen = true;
            }
        }
        if seen {
            break;
        }
    }
    assert!(seen, "awareness update was not relayed");

    // Disconnecting A must broadcast the removal, not only drop it locally (§10, README:
    // awareness is dropped when a connection closes).
    drop(a);
    let mut removed = false;
    for _ in 0..10 {
        let Some(frames) = try_recv(&mut b, Duration::from_millis(300)).await else {
            break;
        };
        for frame in frames {
            if let Message::Awareness(update) = frame {
                if let Some(entry) = update.clients.get(&yrs::block::ClientID::new(4242)) {
                    if entry.json.to_string() == "null" {
                        removed = true;
                    }
                }
            }
        }
        if removed {
            break;
        }
    }
    assert!(removed, "the awareness removal was not broadcast");

    // Awareness never becomes persisted content.
    assert_eq!(get(&state, &token, "/private/alice/aw.md").await, "x");
}

#[tokio::test]
async fn deletion_is_pushed_to_connected_clients() {
    use meisync_api::FileService;

    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/gone.md", "bye").await;

    let mut a = connect(&server, &token, "/private/alice/gone.md").await;
    let doc_a = Doc::new();
    handshake(&mut a, &doc_a).await;

    let user = state.auth.authenticate(&token).await.unwrap();
    state
        .files
        .delete(&user, "/private/alice/gone.md")
        .await
        .expect("delete");

    let mut notified = false;
    for _ in 0..8 {
        let frames = recv(&mut a).await;
        assert!(!frames.is_empty(), "empty protocol frame");
        for frame in frames {
            if let Message::Auth(Some(reason)) = frame {
                assert_eq!(reason, "deleted");
                notified = true;
            }
        }
        if notified {
            break;
        }
    }
    assert!(notified, "client was not told that the file is gone");
}

#[tokio::test]
async fn rename_is_pushed_to_connected_clients() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/old.md", "stable").await;

    let mut a = connect(&server, &token, "/private/alice/old.md").await;
    let doc_a = Doc::new();
    handshake(&mut a, &doc_a).await;

    let request = Request::builder()
        .method("MOVE")
        .uri("/dav/private/alice/old.md")
        .header("authorization", format!("Bearer {token}"))
        .header("destination", "/dav/private/alice/new.md")
        .body(Body::empty())
        .unwrap();
    let app = meisync_server::router(state.clone());
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("move response");
    assert_eq!(response.status(), StatusCode::CREATED);

    let mut notified = false;
    for _ in 0..8 {
        let frames = recv(&mut a).await;
        assert!(!frames.is_empty(), "empty protocol frame");
        for frame in frames {
            if let Message::Custom(tag, data) = frame {
                assert_eq!(tag, meisync_ysync::MSG_DOC_MOVED);
                assert_eq!(String::from_utf8_lossy(&data), "/private/alice/new.md");
                notified = true;
            }
        }
        if notified {
            break;
        }
    }
    assert!(notified, "client was not told about the move");
    // The YDoc itself is untouched by a rename (§29).
    assert_eq!(get(&state, &token, "/private/alice/new.md").await, "stable");
}

#[tokio::test]
async fn ysync_requires_a_valid_token() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/notes.md", "hello").await;

    // No token.
    let url = format!("ws://{}/ysync/private/alice/notes.md", server.addr);
    let result = tokio_tungstenite::connect_async(url).await;
    assert!(result.is_err(), "handshake without a token must fail");

    // Wrong token.
    let url = format!(
        "ws://{}/ysync/private/alice/notes.md?token=nope",
        server.addr
    );
    assert!(tokio_tungstenite::connect_async(url).await.is_err());

    // Foreign user's private file.
    let bob = register(&state, "bob").await;
    let url = format!(
        "ws://{}/ysync/private/alice/notes.md?token={bob}",
        server.addr
    );
    assert!(
        tokio_tungstenite::connect_async(url).await.is_err(),
        "bob must not be able to sync alice's private document"
    );
}

#[tokio::test]
async fn ysync_updates_are_persisted_across_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let data_dir = dir.path().to_path_buf();

    let token;
    let addr;
    {
        let config = meisync_api::ServerConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        let state = meisync_api::AppState::bootstrap(config).await.unwrap();
        token = register(&state, "alice").await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        addr = listener.local_addr().unwrap();
        let app = meisync_server::router(state.clone());
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let server = YSyncServerRef(addr);
        put(&state, &token, "/private/alice/notes.md", "").await;

        let mut client = connect_ref(&server, &token, "/private/alice/notes.md").await;
        let mut doc = Doc::new();
        handshake(&mut client, &doc).await;
        send(
            &mut client,
            Message::Sync(SyncMessage::Update(edit(&mut doc, 0, "synced "))),
        )
        .await;
        // Give the server a moment to persist the update.
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(client);
        handle.abort();
    }

    // Restart on the same data directory: the update must still be there (§17, §18).
    let config = meisync_api::ServerConfig {
        data_dir,
        ..Default::default()
    };
    let state = meisync_api::AppState::bootstrap(config).await.unwrap();
    assert_eq!(
        get(&state, &token, "/private/alice/notes.md").await,
        "synced ",
        "update log must be replayed on restart"
    );
}

struct YSyncServerRef(std::net::SocketAddr);

async fn connect_ref(server: &YSyncServerRef, token: &str, path: &str) -> Client {
    let url = format!("ws://{}/ysync{path}?token={token}", server.0);
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio_tungstenite::connect_async(url).await
    })
    .await
    .expect("connect timeout")
    .expect("handshake");
    socket
}
#[tokio::test]
async fn a_read_only_peer_cannot_write_a_public_document() {
    let (server, state) = YSyncServer::spawn().await;
    let alice = register(&state, "alice").await;
    let bob = register(&state, "bob").await;
    put(&state, &alice, "/public/shared.md", "hello").await;

    // Public = read + sync for every account, so bob may join the room and read …
    let mut bob_client = connect(&server, &bob, "/public/shared.md").await;
    let mut bob_doc = Doc::new();
    handshake(&mut bob_client, &bob_doc).await;
    assert_eq!(content_of(&bob_doc), "hello");

    // … but writing needs `WRITE` (owner or an explicit ACL grant, §12).
    let update = edit(&mut bob_doc, 5, " hacked");
    send(&mut bob_client, Message::Sync(SyncMessage::Update(update))).await;
    let mut denied = false;
    for _ in 0..10 {
        let Some(frames) = try_recv(&mut bob_client, Duration::from_millis(300)).await else {
            break;
        };
        for frame in frames {
            if let Message::Auth(Some(reason)) = frame {
                assert!(reason.contains("write"), "{reason}");
                denied = true;
            }
        }
        if denied {
            break;
        }
    }
    assert!(denied, "a read-only peer must be told that its update was refused");
    assert_eq!(
        get(&state, &alice, "/public/shared.md").await,
        "hello",
        "a read-only peer must not be able to rewrite the document"
    );

    // The owner writes the very same document without any problem.
    let mut alice_client = connect(&server, &alice, "/public/shared.md").await;
    let mut alice_doc = Doc::new();
    handshake(&mut alice_client, &alice_doc).await;
    let update = edit(&mut alice_doc, 5, " world");
    send(&mut alice_client, Message::Sync(SyncMessage::Update(update))).await;
    let mut written = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if get(&state, &alice, "/public/shared.md").await == "hello world" {
            written = true;
            break;
        }
    }
    assert!(written, "the owner must be able to write");
}

/// A CRDT edit must reach the file-level change feed.
///
/// Closed copies of a note (another Obsidian vault, a WebDAV/S3 mirror, the admin console) only
/// learn about remote changes through `ChangeHub`. YSync writes bypass `FileService`, so without
/// an explicit announcement those clients stayed stale until their next manifest poll.
#[tokio::test]
async fn ysync_edits_are_published_on_the_file_change_feed() {
    let (server, state) = YSyncServer::spawn().await;
    let token = register(&state, "alice").await;
    put(&state, &token, "/private/alice/notes.md", "hello").await;

    // Only events caused by the YSync edit matter here.
    let mut changes = state.changes.subscribe();
    while changes.try_recv().is_ok() {}

    let mut a = connect(&server, &token, "/private/alice/notes.md").await;
    let mut doc_a = Doc::new();
    handshake(&mut a, &doc_a).await;

    // The handshake's empty `SyncStep2` is not a write and must stay silent.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), changes.recv())
            .await
            .is_err(),
        "an up-to-date client answering SyncStep1 must not announce a change"
    );

    let update = edit(&mut doc_a, 5, " world");
    send(&mut a, Message::Sync(SyncMessage::Update(update))).await;

    let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("a ysync edit must reach the change feed")
        .expect("change");
    assert_eq!(change.kind, meisync_api::ChangeKind::Updated);
    assert_eq!(change.path, "/private/alice/notes.md");
    assert!(!change.collection);
    assert_eq!(change.size, 11);
    assert!(change.etag.is_some(), "the event must carry the new ETag");
}
