//! Obsidian sync API coverage: vault provisioning, manifest, ETag conflicts, move/delete and devices.

mod common;

use axum::http::StatusCode;
use common::{body_bytes, body_string, CloneHeader, Harness};
use futures_util::StreamExt;

fn sync_url(action: &str, vault: &str, path: &str) -> String {
    format!(
        "/api/sync/{action}?vault={}&path={}",
        urlencode(vault),
        urlencode(path)
    )
}

fn urlencode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

async fn handshake(harness: &Harness, token: &str, vault: &str) -> serde_json::Value {
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/sync/handshake",
                Some(token),
                Some("application/json"),
                serde_json::to_vec(&serde_json::json!({
                    "vault": vault,
                    "device_id": "test-device",
                    "name": "Test device",
                    "platform": "test",
                    "plugin_version": "0.1.0"
                }))
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_str(&body_string(response).await).unwrap()
}

#[tokio::test]
async fn upload_manifest_conflict_move_delete_and_devices() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let hs = handshake(&harness, &token, "obsidian/MyVault").await;
    assert_eq!(hs["root"], "/private/alice/obsidian/MyVault");
    assert_eq!(hs["user"]["username"], "alice");

    // Create-only upload.
    let response = harness
        .send(
            harness
                .request_with_body(
                    "PUT",
                    &sync_url("file", "obsidian/MyVault", "notes/hello.md"),
                    Some(&token),
                    Some("text/markdown"),
                    "# hello",
                )
                .clone_header("if-none-match", "*"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let uploaded: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let etag = uploaded["etag"].as_str().unwrap().to_string();
    assert_eq!(uploaded["sync_mode"], "incremental");

    // Manifest paths are relative to the vault root and include the collection.
    let response = harness
        .send(harness.request(
            "GET",
            "/api/sync/manifest?vault=obsidian%2FMyVault",
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let manifest: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry["path"] == "notes" && entry["kind"] == "collection"));
    let hello = entries
        .iter()
        .find(|entry| entry["path"] == "notes/hello.md")
        .unwrap();
    assert_eq!(hello["etag"], etag);
    assert_eq!(hello["kind"], "file");

    // Full content and conditional GET.
    let response = harness
        .send(harness.request(
            "GET",
            &sync_url("file", "obsidian/MyVault", "notes/hello.md"),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, b"# hello");
    let response = harness
        .send(
            harness
                .request(
                    "GET",
                    &sync_url("file", "obsidian/MyVault", "notes/hello.md"),
                    Some(&token),
                )
                .clone_header("if-none-match", &etag),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // Wrong If-Match is a 412; the correct one wins.
    let response = harness
        .send(
            harness
                .request_with_body(
                    "PUT",
                    &sync_url("file", "obsidian/MyVault", "notes/hello.md"),
                    Some(&token),
                    Some("text/markdown"),
                    "# nope",
                )
                .clone_header("if-match", "\"wrong\""),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    let response = harness
        .send(
            harness
                .request_with_body(
                    "PUT",
                    &sync_url("file", "obsidian/MyVault", "notes/hello.md"),
                    Some(&token),
                    Some("text/markdown"),
                    "# hello world",
                )
                .clone_header("if-match", &etag),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_ne!(updated["etag"], etag);

    // Folder creation is idempotent.
    for _ in 0..2 {
        let response = harness
            .send(
                harness.request_with_body(
                    "POST",
                    "/api/sync/mkdir",
                    Some(&token),
                    Some("application/json"),
                    serde_json::to_vec(&serde_json::json!({
                        "vault": "obsidian/MyVault",
                        "path": "empty/folder"
                    }))
                    .unwrap(),
                ),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    // Move/rename keeps the same content but changes the manifest path.
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/sync/move",
                Some(&token),
                Some("application/json"),
                serde_json::to_vec(&serde_json::json!({
                    "vault": "obsidian/MyVault",
                    "from": "notes/hello.md",
                    "to": "notes/renamed.md"
                }))
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = harness
        .send(harness.request(
            "GET",
            "/api/sync/manifest?vault=obsidian%2FMyVault",
            Some(&token),
        ))
        .await;
    let manifest: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry["path"] == "notes/renamed.md"));
    assert!(!entries
        .iter()
        .any(|entry| entry["path"] == "notes/hello.md"));

    // Device registration is visible through the API.
    let response = harness
        .send(harness.request("GET", "/api/sync/devices", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let devices: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert!(devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["device_id"] == "test-device"));

    // Delete with the current ETag.
    let response = harness
        .send(harness.request(
            "GET",
            "/api/sync/manifest?vault=obsidian%2FMyVault",
            Some(&token),
        ))
        .await;
    let manifest: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let renamed = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "notes/renamed.md")
        .unwrap()
        .clone();
    let response = harness
        .send(
            harness
                .request(
                    "DELETE",
                    &sync_url("file", "obsidian/MyVault", "notes/renamed.md"),
                    Some(&token),
                )
                .clone_header("if-match", renamed["etag"].as_str().unwrap()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_evaluates_if_match_against_the_live_document() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    handshake(&harness, &token, "obsidian/MyVault").await;

    // Two writes: the second ETag is the revision a client has to present to delete it.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
            Some(&token),
            Some("text/markdown"),
            "first",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let first: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
            Some(&token),
            Some("text/markdown"),
            "second",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let second: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_ne!(first["etag"], second["etag"]);

    // A stale If-Match must not delete the newer content (§25).
    let response = harness
        .send(
            harness
                .request(
                    "DELETE",
                    &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
                    Some(&token),
                )
                .clone_header("if-match", first["etag"].as_str().unwrap()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    let response = harness
        .send(harness.request(
            "GET",
            &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "second");

    // The current ETag is accepted.
    let response = harness
        .send(
            harness
                .request(
                    "DELETE",
                    &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
                    Some(&token),
                )
                .clone_header("if-match", second["etag"].as_str().unwrap()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = harness
        .send(harness.request(
            "GET",
            &sync_url("file", "obsidian/MyVault", "notes/stale.md"),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn vaults_are_scoped_to_the_authenticated_account() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;
    handshake(&harness, &alice, "obsidian/AliceVault").await;

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &sync_url("file", "obsidian/AliceVault", "secret.md"),
            Some(&alice),
            Some("text/markdown"),
            "alice only",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Bob cannot address Alice's namespace through a traversal or an absolute path.
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/sync/handshake",
            Some(&bob),
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"vault": "../../private/alice"})).unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/sync/handshake",
            Some(&bob),
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"vault": "/private/alice"})).unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let hs: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(hs["root"], "/private/bob/private/alice");
    let response = harness
        .send(harness.request(
            "GET",
            &sync_url("file", "private/alice", "secret.md"),
            Some(&bob),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Alice's own manifest is not visible through Bob's vault root.
    let response = harness
        .send(harness.request(
            "GET",
            "/api/sync/manifest?vault=obsidian%2FAliceVault",
            Some(&bob),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let manifest: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert!(manifest["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn sync_info_advertises_the_protocol() {
    let harness = Harness::spawn().await;
    let response = harness
        .send(harness.request("GET", "/api/sync/info", None))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["service"], "meisync");
    assert_eq!(value["protocol"], 1);
    assert!(value["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry == "manifest"));
}

#[tokio::test]
async fn websocket_feed_announces_file_changes() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    handshake(&harness, &token, "obsidian/MyVault").await;

    // A real listener is required for WebSocket upgrades; the in-process router keeps sharing the
    // same AppState, so HTTP mutations are visible to the socket.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = harness.app.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let url = format!(
        "ws://{addr}/api/sync/ws?vault={}&token={}",
        urlencode("obsidian/MyVault"),
        urlencode(&token)
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("websocket");
    let ready = socket.next().await.expect("ready").expect("ready frame");
    let ready_text = ready.into_text().expect("text");
    let ready: serde_json::Value = serde_json::from_str(&ready_text).unwrap();
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["root"], "/private/alice/obsidian/MyVault");

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &sync_url("file", "obsidian/MyVault", "live.md"),
            Some(&token),
            Some("text/markdown"),
            "# live",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let change = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("change timeout")
            .expect("change frame")
            .expect("change message");
        if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "change" {
                break value;
            }
        }
    };
    assert_eq!(change["type"], "change");
    assert_eq!(change["change"]["kind"], "created");
    assert_eq!(
        change["change"]["path"],
        "/private/alice/obsidian/MyVault/live.md"
    );

    let _ = socket.close(None).await;
    server.abort();
}
