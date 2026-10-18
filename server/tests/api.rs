//! Control-plane coverage: accounts, tokens and the JSON file API.

mod common;

use axum::http::StatusCode;
use common::{body_string, Harness};

#[tokio::test]
async fn register_login_and_identity() {
    let harness = Harness::spawn().await;

    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/register",
            None,
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"username": "alice", "password": "secret123"}))
                .unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert!(value["token"].as_str().unwrap().len() >= 32);
    assert_eq!(value["user"]["username"], "alice");
    // The password hash is never exposed.
    assert!(value.to_string().find("password_hash").is_none());

    // Duplicate registration is a conflict.
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/register",
            None,
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"username": "alice", "password": "secret123"}))
                .unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // Wrong password -> 401.
    let response = harness
        .send(harness.request_with_body(
            "POST",
            "/api/auth/login",
            None,
            Some("application/json"),
            serde_json::to_vec(&serde_json::json!({"username": "alice", "password": "nope"}))
                .unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Valid login -> token, /me works.
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
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let token = value["token"].as_str().unwrap().to_string();

    let response = harness
        .send(harness.request("GET", "/api/me", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["username"], "alice");

    // Logging out invalidates the token.
    let response = harness
        .send(harness.request("POST", "/api/auth/logout", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request("GET", "/api/me", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn password_hashes_are_not_plaintext() {
    let harness = Harness::spawn().await;
    harness.register("alice").await;
    let rows = harness
        .state
        .storage
        .db
        .call(|conn| {
            let hash: String = conn.query_row(
                "SELECT password_hash FROM users WHERE username = 'alice'",
                [],
                |row| row.get(0),
            ).map_err(|err| meisync_core::MeiSyncError::Storage(err.to_string()))?;
            Ok(hash)
        })
        .await
        .expect("query");
    assert!(rows.starts_with("$argon2"), "{rows}");
    assert!(!rows.contains("secret123"));
}

#[tokio::test]
async fn file_api_creates_updates_and_deletes() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // PUT /api/files/<path>?visibility=public
    let request = harness
        .request_with_body(
            "PUT",
            "/api/files/private/alice/note.md?visibility=public",
            Some(&token),
            Some("text/markdown"),
            "created through the api",
        );
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::OK, "{}", body_string(response).await);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["path"], "/private/alice/note.md");
    assert_eq!(value["kind"], "file");
    assert_eq!(value["visibility"], "public");
    let file_id = value["id"].as_str().unwrap().to_string();

    // GET returns the metadata with a cached size and ETag.
    let response = harness
        .send(harness.request("GET", "/api/files/private/alice/note.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["size"], 23);
    assert!(value["etag"].as_str().unwrap().starts_with('"'));

    // The same file is reachable through WebDAV.
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/note.md", Some(&token)))
        .await;
    assert_eq!(body_string(response).await, "created through the api");

    // Listing with depth=infinity returns the node plus its descendants.
    let response = harness
        .send(harness.request(
            "GET",
            "/api/files/private/alice?depth=infinity",
            Some(&token),
        ))
        .await;
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let array = value.as_array().expect("array");
    assert_eq!(array.len(), 2, "{value}");

    // The ACL of a fresh file is empty and only the owner can read it.
    let response = harness
        .send(harness.request("GET", &format!("/api/acl/{file_id}"), Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "[]");
    let bob = harness.register("bob").await;
    let response = harness
        .send(harness.request("GET", &format!("/api/acl/{file_id}"), Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // DELETE removes it everywhere.
    let response = harness
        .send(harness.request(
            "DELETE",
            "/api/files/private/alice/note.md",
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request("GET", "/dav/private/alice/note.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn document_endpoint_returns_metadata_and_ydoc_content() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/aggregate.md",
            Some(&token),
            Some("text/markdown"),
            "metadata + ydoc",
        ))
        .await;

    let response = harness
        .send(harness.request(
            "GET",
            "/api/document/private/alice/aggregate.md",
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["path"], "/private/alice/aggregate.md");
    assert_eq!(value["collection"], false);
    assert_eq!(value["content"], "metadata + ydoc");
    assert_eq!(value["metadata"]["sync_mode"], "incremental");
    assert!(value["etag"].as_str().is_some() || value["metadata"]["etag"].as_str().is_some());

    // The root is reported as a collection instead of failing.
    let response = harness
        .send(harness.request("GET", "/api/document", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["collection"], true);

    // Another user cannot read a private document.
    let bob = harness.register("bob").await;
    let response = harness
        .send(harness.request(
            "GET",
            "/api/document/private/alice/aggregate.md",
            Some(&bob),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn namespace_bootstrap_matches_the_design() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    // `/` contains the `private` and `public` collections (§4).
    let response = harness
        .send(harness.request("GET", "/api/files/?depth=1", Some(&alice)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let names: Vec<String> = value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_string())
        .filter(|name| !name.is_empty()) // the namespace root itself
        .collect();
    assert_eq!(names, vec!["private".to_string(), "public".to_string()]);

    // Every user has a private namespace and a bucket named after them (§4, §30).
    for (token, username) in [(&alice, "alice"), (&bob, "bob")] {
        let response = harness
            .send(harness.request(
                "GET",
                "/api/files/private?depth=1",
                Some(token),
            ))
            .await;
        let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
        let names: Vec<String> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_string())
            .collect();
        // `private` itself plus the caller's own namespace only.
        assert_eq!(names.len(), 2, "each user only sees their own namespace: {names:?}");
        assert!(names.contains(&username.to_string()), "{names:?}");
    }

    let response = harness.send(harness.request("GET", "/s3", Some(&bob))).await;
    let xml = body_string(response).await;
    assert!(xml.contains("<Name>bob</Name>"), "{xml}");
    assert!(!xml.contains("<Name>alice</Name>"), "{xml}");
}

#[tokio::test]
async fn invalid_tokens_and_paths_are_rejected() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    let response = harness
        .send(harness.request("GET", "/api/files/private/alice", None))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Path traversal in a name is refused.
    let request = harness
        .request_with_body(
            "PUT",
            "/dav/private/alice/..%2F..%2Fescape.md",
            Some(&token),
            Some("text/plain"),
            "nope",
        );
    let response = harness.send(request).await;
    assert!(response.status().is_client_error(), "{:?}", response.status());

    // `..` inside a path is rejected by the name validation.
    let response = harness
        .send(harness.request("MKCOL", "/dav/private/alice/..", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn health_and_index_are_public() {
    let harness = Harness::spawn().await;
    for path in ["/", "/health", "/api/health"] {
        let response = harness.send(harness.request("GET", path, None)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let body = body_string(harness.send(harness.request("GET", "/", None)).await).await;
    assert!(body.contains("/dav"));
    assert!(body.contains("/s3"));
    assert!(body.contains("/ysync"));
}

#[tokio::test]
async fn public_default_write_flag_can_be_enabled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = meisync_api::ServerConfig {
        data_dir: dir.path().to_path_buf(),
        public_default_write: true,
        ..Default::default()
    };
    let state = meisync_api::AppState::bootstrap(config).await.expect("bootstrap");
    let alice = {
        let user = state.auth.register("alice", "secret123").await.unwrap();
        state
            .files
            .ensure_user_namespace(&user.user.id, &user.user.username)
            .await
            .unwrap();
        user.token
    };
    let bob = {
        let user = state.auth.register("bob", "secret123").await.unwrap();
        state
            .files
            .ensure_user_namespace(&user.user.id, &user.user.username)
            .await
            .unwrap();
        user.token
    };

    use meisync_api::FileService;
    let alice_user = state.auth.authenticate(&alice).await.unwrap();
    let bob_user = state.auth.authenticate(&bob).await.unwrap();
    state
        .files
        .replace_content(
            &alice_user,
            "/private/alice/open.md",
            b"public content".to_vec(),
            None,
        )
        .await
        .unwrap();
    let file_id = state
        .files
        .get(&alice_user, "/private/alice/open.md")
        .await
        .unwrap()
        .id;
    state
        .files
        .set_visibility(&alice_user, &file_id, meisync_core::Visibility::Public)
        .await
        .unwrap();

    // With the flag enabled every authenticated user may write public files (§12).
    state
        .files
        .replace_content(
            &bob_user,
            "/private/alice/open.md",
            b"written by bob".to_vec(),
            None,
        )
        .await
        .expect("public write allowed");

    let snapshot = state
        .files
        .read_content(&alice_user, "/private/alice/open.md")
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&snapshot.content), "written by bob");
}

#[tokio::test]
async fn s3_and_webdav_share_one_bucket_namespace() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // Create the bucket through S3 …
    let response = harness.send(harness.request("PUT", "/s3/team", Some(&token))).await;
    assert_eq!(response.status(), StatusCode::OK);

    // … write through WebDAV …
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/team/plan.md",
            Some(&token),
            Some("text/markdown"),
            "# plan",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // … and read through S3.
    let response = harness
        .send(harness.request("GET", "/s3/team/plan.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "# plan");
}
#[tokio::test]
async fn acl_endpoints_answer_404_for_an_unknown_document() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // An unknown id used to panic (`find_by_id(...).unwrap()`) instead of answering 404.
    for body in [
        serde_json::json!({"visibility": "public", "permissions": []}),
        serde_json::json!({"username": "bob", "permissions": ["read"]}),
    ] {
        let request = harness.request_with_body(
            "POST",
            "/api/acl/does-not-exist",
            Some(&token),
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
        );
        let response = harness.send(request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{body}");
    }
}

#[tokio::test]
async fn api_errors_are_json() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    let response = harness
        .send(harness.request("GET", "/api/files/private/alice/missing.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "the JSON body must keep its content type"
    );
    let body = body_string(response).await;
    let value: serde_json::Value = serde_json::from_str(&body).expect("json error body");
    assert_eq!(value["error"], "not_found");
}
