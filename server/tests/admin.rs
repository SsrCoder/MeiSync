//! Administration API coverage: roles, user lifecycle, sessions, audit and file browsing.

mod common;

use axum::http::StatusCode;
use common::{body_string, Harness};

async fn admin_login(harness: &Harness, username: &str) -> String {
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/admin/login",
                None,
                Some("application/json"),
                serde_json::to_vec(&serde_json::json!({
                    "username": username,
                    "password": "secret123"
                }))
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "admin login {username}");
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["role"], "admin");
    value["token"].as_str().expect("token").to_string()
}

async fn json(
    harness: &Harness,
    method: &str,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = harness
        .send(harness.request_with_body(
            method,
            path,
            Some(token),
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
        .await;
    let status = response.status();
    let text = body_string(response).await;
    let value = if text.is_empty() {
        serde_json::json!(null)
    } else {
        serde_json::from_str(&text).unwrap_or(serde_json::json!(text))
    };
    (status, value)
}

#[tokio::test]
async fn first_account_is_admin_and_admin_login_works() {
    let harness = Harness::spawn().await;
    let _root = harness.register("root").await;
    let token = admin_login(&harness, "root").await;

    let response = harness
        .send(harness.request("GET", "/api/admin/overview", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["stats"]["users"], 1);
    assert_eq!(value["stats"]["admins"], 1);
    assert!(value["server"]["version"].as_str().is_some());

    // The console itself is embedded and served without auth (the API is what is protected).
    let response = harness.send(harness.request("GET", "/admin", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_string(response).await;
    assert!(html.contains("MeiSync"));
}

#[tokio::test]
async fn non_admin_cannot_use_the_admin_api() {
    let harness = Harness::spawn().await;
    let _root = harness.register("root").await;
    let bob = harness.register("bob").await;

    let response = harness
        .send(harness.request("GET", "/api/admin/overview", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Logging in through the admin endpoint must not mint a token for a normal user.
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/admin/login",
                None,
                Some("application/json"),
                serde_json::to_vec(&serde_json::json!({
                    "username": "bob",
                    "password": "secret123"
                }))
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn user_lifecycle_disable_reset_delete_and_purge() {
    let harness = Harness::spawn().await;
    let _root = harness.register("root").await;
    let admin = admin_login(&harness, "root").await;

    let (status, created) = json(
        &harness,
        "POST",
        "/api/admin/users",
        &admin,
        serde_json::json!({"username": "charlie", "password": "secret123", "role": "user"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let charlie_id = created["id"].as_str().unwrap().to_string();

    // The account is provisioned with its private namespace.
    let charlie = harness.login("charlie").await;
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/api/files/private/charlie/notes.md",
            Some(&charlie),
            Some("text/markdown"),
            "# hello",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Disabling revokes every token immediately.
    let (status, updated) = json(
        &harness,
        "PATCH",
        &format!("/api/admin/users/{charlie_id}"),
        &admin,
        serde_json::json!({"disabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["disabled"], true);

    let response = harness
        .send(harness.request("GET", "/api/me", Some(&charlie)))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/auth/login",
                None,
                Some("application/json"),
                serde_json::to_vec(
                    &serde_json::json!({"username": "charlie", "password": "secret123"}),
                )
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Re-enable and reset the password.
    let (status, _) = json(
        &harness,
        "PATCH",
        &format!("/api/admin/users/{charlie_id}"),
        &admin,
        serde_json::json!({"disabled": false, "password": "newsecret123"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let response = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/auth/login",
                None,
                Some("application/json"),
                serde_json::to_vec(
                    &serde_json::json!({"username": "charlie", "password": "secret123"}),
                )
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let charlie = harness
        .send(
            harness.request_with_body(
                "POST",
                "/api/auth/login",
                None,
                Some("application/json"),
                serde_json::to_vec(
                    &serde_json::json!({"username": "charlie", "password": "newsecret123"}),
                )
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(charlie.status(), StatusCode::OK);

    // Deleting with purge removes the private namespace.
    let response = harness
        .send(harness.request(
            "DELETE",
            &format!("/api/admin/users/{charlie_id}?purge=true"),
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request(
            "GET",
            "/api/admin/files?path=/private/charlie",
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = harness
        .send(harness.request("GET", "/api/admin/audit?limit=100", Some(&admin)))
        .await;
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let actions = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["action"].as_str())
        .collect::<Vec<_>>();
    assert!(actions.contains(&"admin.user.create"));
    assert!(actions.contains(&"admin.user.update"));
    assert!(actions.contains(&"admin.user.delete"));
}

#[tokio::test]
async fn admin_can_browse_and_delete_any_namespace() {
    let harness = Harness::spawn().await;
    let root = harness.register("root").await;
    let admin = admin_login(&harness, "root").await;

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/api/files/private/root/demo.md",
            Some(&root),
            Some("text/markdown"),
            "# demo",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request(
            "GET",
            "/api/admin/files?path=/private/root&depth=1",
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let files = value["files"].as_array().unwrap();
    let demo = files
        .iter()
        .find(|file| file["name"] == "demo.md")
        .expect("demo.md");
    assert_eq!(value["owners"][demo["owner_id"].as_str().unwrap()], "root");

    let response = harness
        .send(harness.request(
            "GET",
            "/api/admin/file?path=/private/root/demo.md",
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "# demo");

    // The namespace containers themselves are protected.
    let response = harness
        .send(harness.request("DELETE", "/api/admin/files?path=/private", Some(&admin)))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = harness
        .send(harness.request("DELETE", "/api/admin/files?path=/public", Some(&admin)))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = harness
        .send(harness.request(
            "DELETE",
            "/api/admin/files?path=/private/root/demo.md",
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request(
            "GET",
            "/api/admin/files?path=/private/root/demo.md",
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_can_be_listed_and_revoked() {
    let harness = Harness::spawn().await;
    let user_token = harness.register("root").await;
    let admin = admin_login(&harness, "root").await;

    // The client token is a working credential before it is revoked.
    let response = harness
        .send(harness.request("GET", "/api/me", Some(&user_token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request("GET", "/api/admin/sessions?limit=50", Some(&admin)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    let sessions = value["sessions"].as_array().unwrap();
    // The registration and the console login are both active.
    assert!(sessions.len() >= 2);

    for session in sessions {
        assert!(
            session.get("token").is_none(),
            "the payload must not carry the bearer token: {session}"
        );
        assert!(session["id"].is_string());
        assert!(session["token_prefix"].is_string());
        // No field of a row may authenticate as the account.
        for (key, field) in session.as_object().unwrap() {
            let Some(candidate) = field.as_str() else {
                continue;
            };
            let response = harness
                .send(harness.request("GET", "/api/me", Some(candidate)))
                .await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "field {key} leaks a usable credential"
            );
        }
    }

    // Revoke the client session by its opaque id.
    let prefix = format!("{}…{}", &user_token[..6], &user_token[user_token.len() - 4..]);
    let row = sessions
        .iter()
        .find(|session| session["token_prefix"] == prefix.as_str())
        .expect("the client session is listed");
    let id = row["id"].as_str().unwrap().to_string();
    let response = harness
        .send(harness.request(
            "DELETE",
            &format!("/api/admin/sessions/{id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The client token is dead, the console session of the administrator is not.
    let response = harness
        .send(harness.request("GET", "/api/me", Some(&user_token)))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = harness
        .send(harness.request("GET", "/api/me", Some(&admin)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // An unknown (or already revoked) session id is a 404.
    let response = harness
        .send(harness.request(
            "DELETE",
            &format!("/api/admin/sessions/{id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_create_another_admin_but_cannot_remove_the_last_one() {
    let harness = Harness::spawn().await;
    let _root = harness.register("root").await;
    let admin = admin_login(&harness, "root").await;

    let (status, created) = json(
        &harness,
        "POST",
        "/api/admin/users",
        &admin,
        serde_json::json!({"username": "second", "password": "secret123", "role": "admin"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let second_id = created["id"].as_str().unwrap().to_string();

    // Root cannot demote itself while it is one of two admins? It can while another admin exists.
    let response = harness
        .send(harness.request("GET", "/api/admin/users?search=second", Some(&admin)))
        .await;
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["users"][0]["role"], "admin");

    // Deleting the second admin leaves exactly one administrator.
    let response = harness
        .send(harness.request(
            "DELETE",
            &format!("/api/admin/users/{second_id}?purge=false"),
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The last admin cannot delete itself through the API.
    let me: serde_json::Value = serde_json::from_str(
        &body_string(
            harness
                .send(harness.request("GET", "/api/admin/me", Some(&admin)))
                .await,
        )
        .await,
    )
    .unwrap();
    let root_id = me["user"]["id"].as_str().unwrap();
    let response = harness
        .send(harness.request(
            "DELETE",
            &format!("/api/admin/users/{root_id}?purge=false"),
            Some(&admin),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn configured_admin_is_created_at_boot() {
    use axum::body::Body;
    use tower::ServiceExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let config = meisync_api::ServerConfig {
        data_dir: dir.path().to_path_buf(),
        admin_user: Some("boss".into()),
        admin_password: Some("secret123".into()),
        ..Default::default()
    };
    let state = meisync_server::bootstrap(config).await.expect("bootstrap");
    let app = meisync_server::router(state);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/admin/login")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "username": "boss",
                "password": "secret123"
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["role"], "admin");

    // The configured admin namespace is provisioned as well.
    let token = value["token"].as_str().unwrap();
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/admin/files?path=/private/boss")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
}
