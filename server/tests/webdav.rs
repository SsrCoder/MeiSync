//! End-to-end WebDAV coverage (`docs/mvp.md` §7, §25, §26).

mod common;

use axum::http::StatusCode;
use common::{body_bytes, body_string, CloneHeader, Harness};
use meisync_api::FileService;

#[tokio::test]
async fn full_lifecycle_over_webdav() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // MKCOL
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/alice/notes", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // PUT creates the file and stores the content in the YDoc.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/notes/todo.md",
            Some(&token),
            Some("text/markdown"),
            "# TODO\n- task 1",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_string();

    // GET materialises the YDoc again.
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/notes/todo.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "# TODO\n- task 1");

    // If-None-Match -> 304 (WebDAV polling, §26)
    let request = harness
        .request("GET", "/dav/private/alice/notes/todo.md", Some(&token))
        .clone_header("if-none-match", &etag);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // If-Match with a stale ETag -> 412 (§25)
    let request = harness
        .request("PUT", "/dav/private/alice/notes/todo.md", Some(&token))
        .clone_header("if-match", "\"stale\"");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    // If-Match with the current ETag -> accepted.
    let request = harness
        .request_with_body(
            "PUT",
            "/dav/private/alice/notes/todo.md",
            Some(&token),
            Some("text/markdown"),
            "# TODO\n- task 1\n- task 2",
        )
        .clone_header("if-match", &etag);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // MOVE keeps the same FileId: the content is untouched (§29).
    let request = harness
        .request("MOVE", "/dav/private/alice/notes/todo.md", Some(&token))
        .clone_header("destination", "/dav/private/alice/notes/renamed.md");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = harness
        .send(harness.request("GET", "/dav/private/alice/notes/renamed.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "# TODO\n- task 1\n- task 2");

    // COPY produces an independent file with the same content.
    let request = harness
        .request("COPY", "/dav/private/alice/notes/renamed.md", Some(&token))
        .clone_header("destination", "/dav/private/alice/notes/copy.md");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/notes/copy.md", Some(&token)))
        .await;
    assert_eq!(body_string(response).await, "# TODO\n- task 1\n- task 2");

    // A change to the copy does not affect the original.
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/notes/copy.md",
            Some(&token),
            Some("text/markdown"),
            "other",
        ))
        .await;
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/notes/renamed.md", Some(&token)))
        .await;
    assert_eq!(body_string(response).await, "# TODO\n- task 1\n- task 2");

    // DELETE removes the file and its YDoc artefacts (§28).
    let response = harness
        .send(harness.request("DELETE", "/dav/private/alice/notes/copy.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/notes/copy.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn propfind_lists_namespace() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request("MKCOL", "/dav/private/alice/notes", Some(&token)))
        .await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/notes/a.md",
            Some(&token),
            Some("text/markdown"),
            "a",
        ))
        .await;

    let request = harness
        .request("PROPFIND", "/dav/private/alice/notes", Some(&token))
        .clone_header("depth", "1");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::MULTI_STATUS);
    let xml = body_string(response).await;
    assert!(xml.contains("<D:multistatus"), "{xml}");
    assert!(xml.contains("/dav/private/alice/notes/a.md"), "{xml}");
    assert!(xml.contains("<D:collection/>"), "{xml}");
    assert!(xml.contains("text/markdown"), "{xml}");
}

#[tokio::test]
async fn private_files_are_invisible_for_other_users() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/secret.md",
            Some(&alice),
            Some("text/markdown"),
            "top secret",
        ))
        .await;

    let response = harness
        .send(harness.request("GET", "/dav/private/alice/secret.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = harness
        .send(harness.request("PUT", "/dav/private/alice/secret.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Writing into another user's home is refused – and, being invisible, it answers 404
    // rather than 403 (README: private files of other users answer 404, never 403).
    harness
        .send(harness.request("MKCOL", "/dav/private/bob", Some(&alice)))
        .await;
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/bob/evil", Some(&alice)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // DELETE and MOVE of a foreign private file are equally hidden and mutate nothing.
    let response = harness
        .send(harness.request("DELETE", "/dav/private/alice/secret.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let request = harness
        .request("MOVE", "/dav/private/alice/secret.md", Some(&bob))
        .clone_header("destination", "/dav/private/bob/stolen.md");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // … as is MOVE *into* another user's private home.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/bob/mine.md",
            Some(&bob),
            Some("text/markdown"),
            "mine",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let request = harness
        .request("MOVE", "/dav/private/bob/mine.md", Some(&bob))
        .clone_header("destination", "/dav/private/alice/mine.md");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // The source survived every attempt.
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/secret.md", Some(&alice)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "top secret");

    // PROPFIND only shows what the caller may see.
    let request = harness
        .request("PROPFIND", "/dav/private", Some(&bob))
        .clone_header("depth", "1");
    let response = harness.send(request).await;
    let xml = body_string(response).await;
    assert!(xml.contains("/dav/private/bob"), "{xml}");
    assert!(!xml.contains("/dav/private/alice"), "{xml}");
}

#[tokio::test]
async fn public_files_are_shared_and_acl_controls_writing() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    let request = harness
        .request_with_body(
            "PUT",
            "/dav/private/alice/shared.md",
            Some(&alice),
            Some("text/markdown"),
            "shared content",
        )
        .clone_header("x-meisync-visibility", "public");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Public file: any authenticated user may read it (§12).
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/shared.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "shared content");

    // … but not write it: the File Permission layer decides.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/shared.md",
            Some(&bob),
            Some("text/markdown"),
            "hacked",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The owner grants bob write access.
    let file_id = harness
        .state
        .files
        .get(
            &harness
                .state
                .auth
                .authenticate(&alice)
                .await
                .unwrap(),
            "/private/alice/shared.md",
        )
        .await
        .unwrap()
        .id;
    let body = serde_json::json!({"username": "bob", "permissions": ["write", "sync"]});
    let request = harness
        .request_with_body(
            "POST",
            &format!("/api/acl/{file_id}"),
            Some(&alice),
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
        );
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "acl grant");

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/shared.md",
            Some(&bob),
            Some("text/markdown"),
            "bob was here",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // And the write is immediately visible through every protocol.
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/shared.md", Some(&alice)))
        .await;
    assert_eq!(body_string(response).await, "bob was here");
}

#[tokio::test]
async fn children_inherit_the_visibility_of_their_collection() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    // Files created inside `/public` are public (§4) …
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/public/shared.md",
            Some(&alice),
            Some("text/markdown"),
            "for everybody",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = harness
        .send(harness.request("GET", "/dav/public/shared.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "for everybody");

    // … while files inside a private namespace stay private.
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/private.md",
            Some(&alice),
            Some("text/markdown"),
            "only mine",
        ))
        .await;
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/private.md", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mkcol_on_an_existing_resource_is_method_not_allowed() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/file.md",
            Some(&token),
            Some("text/markdown"),
            "x",
        ))
        .await;
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/alice/file.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

    // A missing parent is a conflict, not a 404 (RFC 4918 §8.3.1).
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/alice/a/b", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn recursive_delete_removes_children() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    for path in ["a.md", "sub/b.md", "sub/deep/c.md"] {
        harness
            .send(harness.request_with_body(
                "PUT",
                &format!("/dav/private/alice/{path}"),
                Some(&token),
                Some("text/markdown"),
                path,
            ))
            .await;
    }
    let response = harness
        .send(harness.request("DELETE", "/dav/private/alice/sub", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let request = harness
        .request("PROPFIND", "/dav/private/alice", Some(&token))
        .clone_header("depth", "1");
    let xml = body_string(harness.send(request).await).await;
    assert!(xml.contains("a.md"), "{xml}");
    assert!(!xml.contains("sub"), "{xml}");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let harness = Harness::spawn().await;
    let response = harness
        .send(harness.request("PROPFIND", "/dav/private/alice", None))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = harness
        .send(harness.request("GET", "/dav/private/alice", Some("not-a-token")))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn utf8_content_survives_the_roundtrip() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let content = "# 任务 ✅\nemoji: 🦀\n";
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/任务.md",
            Some(&token),
            Some("text/markdown"),
            content,
        ))
        .await;
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/任务.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, content.as_bytes());
}

/// §5.1/§5.2 storage encoding follows the resolved content type: a *specific* declared type wins,
/// a generic one (`application/octet-stream`, `application/x-www-form-urlencoded`, …) defers to
/// the file extension, so `curl --data-binary` / Apifox / S3 SDK uploads of `.md` files stay text.
#[tokio::test]
async fn a_generic_content_type_defers_to_the_file_extension() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let user = harness.state.auth.authenticate(&token).await.unwrap();

    // The exact Apifox / curl shape: octet-stream on a markdown file must stay plain text,
    // otherwise incremental sync is lost for good.
    let markdown = "# 图床\n\n中文正文\n";
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/图床.md",
            Some(&token),
            Some("application/octet-stream"),
            markdown,
        ))
        .await;
    assert!(response.status().is_success(), "PUT -> {}", response.status());

    let response = harness
        .send(harness.request("GET", "/dav/private/alice/图床.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    // Byte identical: it was never base64 encoded on the way in.
    assert_eq!(body_bytes(response).await, markdown.as_bytes());

    let document = harness
        .state
        .files
        .document(&user, "/private/alice/图床.md")
        .await
        .expect("document");
    assert_eq!(
        document.metadata.sync_mode,
        meisync_core::SyncMode::Incremental,
        "a markdown file uploaded as octet-stream keeps incremental sync"
    );
    assert_eq!(document.metadata.content_type.as_deref(), Some("text/markdown"));
    assert_eq!(document.materialize_string(), markdown);

    // A generic type on a binary file still stores base64 and returns the exact bytes.
    let payload: Vec<u8> = vec![0, 159, 146, 150, 255, 0, 17];
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/logo.png",
            Some(&token),
            Some("application/octet-stream"),
            payload.clone(),
        ))
        .await;
    assert!(response.status().is_success(), "PUT -> {}", response.status());
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/logo.png", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, payload);

    // curl's default for --data-binary is form encoding; it must not turn a markdown file binary.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/curl-default.md",
            Some(&token),
            Some("application/x-www-form-urlencoded"),
            markdown,
        ))
        .await;
    assert!(response.status().is_success(), "PUT -> {}", response.status());
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/curl-default.md", Some(&token)))
        .await;
    assert_eq!(body_bytes(response).await, markdown.as_bytes());
    assert_eq!(
        harness
            .state
            .files
            .get(&user, "/private/alice/curl-default.md")
            .await
            .expect("get")
            .sync_mode,
        meisync_core::SyncMode::Incremental
    );

    // No extension at all: nothing to go by, so stay binary.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/blob",
            Some(&token),
            Some("application/octet-stream"),
            "plain text\n",
        ))
        .await;
    assert!(response.status().is_success(), "PUT -> {}", response.status());
    let meta = harness
        .state
        .files
        .get(&user, "/private/alice/blob")
        .await
        .expect("get");
    assert_eq!(meta.sync_mode, meisync_core::SyncMode::Full);

    // A *specific* declared type still wins over the extension, in both directions.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/declared.bin",
            Some(&token),
            Some("text/plain"),
            "still text\n",
        ))
        .await;
    assert!(response.status().is_success(), "PUT -> {}", response.status());
    let meta = harness
        .state
        .files
        .get(&user, "/private/alice/declared.bin")
        .await
        .expect("get");
    assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
    assert_eq!(meta.sync_mode, meisync_core::SyncMode::Incremental);
    assert_eq!(
        harness
            .state
            .files
            .read_content(&user, "/private/alice/declared.bin")
            .await
            .expect("read")
            .content,
        b"still text\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_conditional_puts_with_one_etag_have_one_winner() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let uri = "/dav/private/alice/race.md";
    harness
        .send(harness.request_with_body(
            "PUT",
            uri,
            Some(&token),
            Some("text/markdown"),
            "v1",
        ))
        .await;
    let response = harness
        .send(harness.request("GET", uri, Some(&token)))
        .await;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_string();

    // Two writers race with the *same* `If-Match`: the precondition has to be evaluated inside
    // the document lock, so exactly one of them may win (§25). `tokio::join!` over two
    // `oneshot` futures would poll them one after the other in a single task; spawned tasks
    // plus a barrier put both requests in flight before either can resolve.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for body in ["a", "b"] {
        let request = harness
            .request_with_body("PUT", uri, Some(&token), Some("text/markdown"), body)
            .clone_header("if-match", &etag);
        let app = harness.app.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            use tower::ServiceExt;
            barrier.wait().await;
            app.oneshot(request)
                .await
                .expect("router response")
                .status()
        }));
    }
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.expect("join"));
    }
    assert!(
        statuses.contains(&StatusCode::NO_CONTENT),
        "one writer must win: {statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::PRECONDITION_FAILED),
        "the loser must see 412: {statuses:?}"
    );
}

#[tokio::test]
async fn copy_keeps_the_bytes_of_a_binary_source() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let payload: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0x00];

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/logo.png",
            Some(&token),
            Some("image/png"),
            payload.clone(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // COPY is byte identical: the destination *name* must not decide the encoding (§7).
    let request = harness
        .request("COPY", "/dav/private/alice/logo.png", Some(&token))
        .clone_header("destination", "/dav/private/alice/logo.txt");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = harness
        .send(harness.request("GET", "/dav/private/alice/logo.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_bytes(response).await,
        payload,
        "COPY must not re-encode the source bytes"
    );
}

#[tokio::test]
async fn a_users_private_namespace_cannot_be_squatted() {
    let harness = Harness::spawn().await;
    let attacker = harness.register("mallory").await;

    // `/private` children are provisioned by the server, so `mallory` cannot occupy
    // `/private/victim` before `victim` registers (§4) – neither directly nor through a PUT.
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/victim", Some(&attacker)))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/victim/x.md",
            Some(&attacker),
            Some("text/markdown"),
            "x",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Neither attempt created anything: `/private` only shows the attacker's own home.
    let request = harness
        .request("PROPFIND", "/dav/private", Some(&attacker))
        .clone_header("depth", "1");
    let xml = body_string(harness.send(request).await).await;
    assert!(!xml.contains("/dav/private/victim"), "{xml}");

    // The name stays usable: registration provisions the home, login works and the home is
    // writable.
    let body = serde_json::json!({"username": "victim", "password": "secret123"});
    let request = harness.request_with_body(
        "POST",
        "/api/auth/register",
        None,
        Some("application/json"),
        serde_json::to_vec(&body).unwrap(),
    );
    let response = harness.send(request).await;
    assert!(
        response.status().is_success(),
        "register -> {}",
        response.status()
    );
    let victim = harness.login("victim").await;

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/victim/hello.md",
            Some(&victim),
            Some("text/markdown"),
            "hello",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = harness
        .send(harness.request("GET", "/dav/private/victim/hello.md", Some(&victim)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "hello");
}

#[tokio::test]
async fn a_rejected_move_into_its_own_subtree_changes_nothing() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // `/private/alice/a` is a collection; `/a/b/a.md` is the MOVE target that the overwrite step
    // of the same request would delete before the illegal-subtree check ran (§29).
    for path in ["/dav/private/alice/a/b/a.md", "/dav/private/alice/a/c.md"] {
        let response = harness
            .send(harness.request_with_body(
                "PUT",
                path,
                Some(&token),
                Some("text/markdown"),
                "keep me",
            ))
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "PUT {path}");
    }

    // WebDAV defaults `Overwrite` to true, so this request reaches the overwrite delete.
    let request = harness
        .request("MOVE", "/dav/private/alice/a", Some(&token))
        .clone_header("destination", "/dav/private/alice/a/b/a.md");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // The rejected MOVE left the overwrite target intact …
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/a/b/a.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "keep me");

    // … and the source subtree unchanged, in place.
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/a/c.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "keep me");
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/a.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_move_cannot_occupy_the_private_container() {
    // `--public-default-write` grants WRITE on the system owned `/private` container, so the
    // squatting refusal has to cover MOVE too, not only MKCOL and PUT (§4).
    let dir = tempfile::tempdir().expect("temp dir");
    let state = meisync_api::AppState::bootstrap(meisync_api::ServerConfig {
        data_dir: dir.path().to_path_buf(),
        public_default_write: true,
        ..Default::default()
    })
    .await
    .expect("bootstrap");
    let harness = Harness {
        app: meisync_server::router(state.clone()),
        state,
        dir: Some(dir),
    };
    let attacker = harness.register("mallory").await;

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/mallory/x.md",
            Some(&attacker),
            Some("text/markdown"),
            "x",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let request = harness
        .request("MOVE", "/dav/private/mallory/x.md", Some(&attacker))
        .clone_header("destination", "/dav/private/victim");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        body_string(response).await.contains("provisioned by the server at registration"),
        "the private root guard must be what refuses the MOVE"
    );

    // Nothing was created under `/private` …
    let request = harness
        .request("PROPFIND", "/dav/private", Some(&attacker))
        .clone_header("depth", "1");
    let xml = body_string(harness.send(request).await).await;
    assert!(!xml.contains("/dav/private/victim"), "{xml}");
    // … and the source is still where it was.
    let response = harness
        .send(harness.request("GET", "/dav/private/mallory/x.md", Some(&attacker)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The name stays usable: `victim` registers and owns a writable home.
    let victim = harness.register("victim").await;
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/victim/hello.md",
            Some(&victim),
            Some("text/markdown"),
            "hello",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn a_home_made_public_is_forced_back_to_private_on_login() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let user = harness.state.auth.authenticate(&token).await.unwrap();
    let home = harness
        .state
        .files
        .get(&user, "/private/alice")
        .await
        .expect("home");

    // The owner publishes their own home …
    let response = harness
        .send(harness.request_with_body(
            "POST",
            &format!("/api/acl/{}", home.id),
            Some(&token),
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"visibility": "public", "permissions": []}))
                .unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let published = harness
        .state
        .files
        .get(&user, "/private/alice")
        .await
        .expect("home");
    assert_eq!(published.visibility, meisync_core::Visibility::Public);

    // … and can still log in: provisioning re-establishes the invariant instead of bricking.
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/login",
            None,
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"username": "alice", "password": "secret123"}))
                .unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let home = harness
        .state
        .files
        .get(&user, "/private/alice")
        .await
        .expect("home");
    assert_eq!(home.visibility, meisync_core::Visibility::Private);
}

#[tokio::test]
async fn a_failed_namespace_provisioning_leaves_no_account_behind() {
    let harness = Harness::spawn().await;
    let attacker = harness.register("mallory").await;
    let mallory = harness.state.auth.authenticate(&attacker).await.unwrap();

    // A `/private/victim` collection that does not belong to the registrant, as an older node
    // (or a direct database write) could have left it behind.
    let mut squatted = harness
        .state
        .files
        .get(&mallory, "/private/mallory")
        .await
        .expect("home");
    squatted.id = meisync_core::FileId::generate();
    squatted.name = "victim".into();
    meisync_persistence::FileRepository::new(harness.state.storage.db.clone())
        .insert(squatted)
        .await
        .expect("insert squatted collection");

    let body = serde_json::json!({"username": "victim", "password": "secret123"});
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/register",
            None,
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // The account was rolled back instead of staying half-created.
    assert!(
        harness
            .state
            .auth
            .users()
            .find_by_username("victim")
            .await
            .expect("query")
            .is_none(),
        "a failed registration must not persist the user"
    );
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/login",
            None,
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
