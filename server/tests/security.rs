//! Security regression tests: nothing may be readable without a *valid* identity.
//!
//! These lock down the behaviour that every entry point (WebDAV, S3 at `/s3`, S3 at the
//! endpoint root, the JSON API and the YSync upgrade) rejects anonymous and invalid tokens.

mod common;

use axum::http::StatusCode;
use common::{body_string, CloneHeader, Harness};

const WRONG: &str = "not-a-real-token-0123456789";

/// "name=visibility" pairs for a namespace path (the node itself included).
async fn vis_of(harness: &Harness, token: &str, path: &str) -> String {
    let response = harness
        .send(harness.request(
            "GET",
            &format!("/api/files{path}?depth=1"),
            Some(token),
        ))
        .await;
    let value: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("json");
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| {
            format!(
                "{}={}",
                item["name"].as_str().unwrap_or("(self)"),
                item["visibility"].as_str().unwrap()
            )
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Seed: alice owns `/photos/test.png` (private), `/private/alice/private.txt` and a public file.
async fn seeded() -> (Harness, String, String) {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;
    for (path, headers) in [
        ("/photos/test.png", vec![]),
        ("/private/alice/private.txt", vec![]),
        ("/public/shared.txt", vec!["x-meisync-visibility: public"]),
    ] {
        let mut request = harness.request_with_body(
            "PUT",
            &format!("/dav{path}"),
            Some(&alice),
            None,
            "TOP SECRET PAYLOAD",
        );
        for header in headers {
            request = request.clone_header(&header.split(':').next().unwrap(), header.split(':').nth(1).unwrap().trim());
        }
        let response = harness.send(request).await;
        assert!(
            response.status().is_success(),
            "setup {path} -> {:?}",
            response.status()
        );
    }
    (harness, alice, bob)
}

#[tokio::test]
async fn anonymous_requests_are_rejected_everywhere() {
    let (harness, _, _) = seeded().await;

    let probes: &[(&str, &str, Option<&str>)] = &[
        ("GET", "/photos/test.png", None),
        ("GET", "/s3/photos/test.png", None),
        ("GET", "/dav/photos/test.png", None),
        ("GET", "/dav/private/alice/private.txt", None),
        ("GET", "/dav/public/shared.txt", None),
        ("HEAD", "/photos/test.png", None),
        ("PROPFIND", "/dav/photos", None),
        ("GET", "/photos?list-type=2", None),
        ("GET", "/api/document/photos/test.png", None),
        ("GET", "/api/files/photos/test.png", None),
        ("GET", "/api/me", None),
        ("GET", "/ysync/photos/test.png?token=bad", None),
    ];

    for (method, uri, token) in probes {
        let request = harness.request(method, uri, *token);
        let response = harness.send(request).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must require authentication"
        );
    }
}

#[tokio::test]
async fn invalid_tokens_are_rejected_everywhere() {
    let (harness, _, _) = seeded().await;

    let probes = [
        ("GET", "/photos/test.png"),
        ("GET", "/s3/photos/test.png"),
        ("GET", "/dav/photos/test.png"),
        ("GET", "/dav/public/shared.txt"),
        ("GET", "/api/files/photos/test.png"),
        ("GET", "/api/document/photos/test.png"),
        ("GET", "/api/me"),
        ("GET", "/s3?list-type=2"),
    ];
    for (method, uri) in probes {
        let response = harness.send(harness.request(method, uri, Some(WRONG))).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must reject a wrong token"
        );
    }

    // … including as a query parameter (WebSocket style) and as a bogus SigV4 signature.
    let response = harness
        .send(harness.request("GET", &format!("/s3/photos/test.png?token={WRONG}"), None))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let request = harness
        .request("GET", "/photos/test.png", None)
        .clone_header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=alice/20240101/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000",
        )
        .clone_header("x-amz-date", "20240101T000000Z");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // An unsigned request must not be treated as ListBuckets at the endpoint root either.
    let response = harness.send(harness.request("GET", "/", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_string(response).await.contains("\"service\":\"meisync\""));
}

#[tokio::test]
async fn other_users_cannot_reach_private_files() {
    let (harness, _, bob) = seeded().await;

    // 404 (not 403) so the namespace does not leak (§12).
    for uri in [
        "/dav/photos/test.png",
        "/s3/photos/test.png",
        "/photos/test.png",
        "/dav/private/alice/private.txt",
        "/s3/alice/private.txt",
        "/api/document/photos/test.png",
    ] {
        let response = harness.send(harness.request("GET", uri, Some(&bob))).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }

    // Listings are filtered as well.
    let response = harness
        .send(harness.request("GET", "/s3/photos?list-type=2", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!body_string(response).await.contains("test.png"));

    // Mutating someone else's private file is refused, not silently ignored.
    for (method, uri) in [
        ("PUT", "/dav/photos/test.png"),
        ("DELETE", "/dav/photos/test.png"),
        ("MKCOL", "/dav/photos/evil"),
    ] {
        let response = harness
            .send(harness.request_with_body(method, uri, Some(&bob), None, "x"))
            .await;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
            ),
            "{method} {uri} -> {:?}",
            response.status()
        );
    }

    // … and the content survived.
    let response = harness
        .send(harness.request("GET", "/dav/photos/test.png", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn public_files_are_readable_by_any_account_but_never_anonymously() {
    let (harness, alice, bob) = seeded().await;

    // Any registered account may read a public file (§12) …
    for token in [&alice, &bob] {
        let response = harness
            .send(harness.request("GET", "/dav/public/shared.txt", Some(token)))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_string(response).await, "TOP SECRET PAYLOAD");
    }

    // … but a public file is still not anonymous content.
    let response = harness
        .send(harness.request("GET", "/dav/public/shared.txt", None))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_in_query_string_is_accepted_but_still_a_secret() {
    let (harness, alice, _) = seeded().await;

    // Browsers and WebSocket clients cannot always set headers, so `?token=` works – which also
    // means a leaked URL leaks access. It is only honoured for valid, non-expired tokens.
    let response = harness
        .send(harness.request("GET", &format!("/dav/photos/test.png?token={alice}"), None))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request("GET", &format!("/dav/photos/test.png?token={WRONG}"), None))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn default_visibility_is_private_except_inside_public_collections() {
    // The agreed semantics: a new node inherits its collection's visibility, and collections are
    // private unless created as public (the `/public` drop box) or marked explicitly.
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    // private: the home namespace
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/a.md",
            Some(&alice),
            None,
            "x",
        ))
        .await;
    assert!(vis_of(&harness, &alice, "/private/alice").await.contains("a.md=private"));

    // private: a top level collection (bucket) and files inside it
    harness
        .send(harness.request("MKCOL", "/dav/photos", Some(&alice)))
        .await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/photos/b.png",
            Some(&alice),
            None,
            "x",
        ))
        .await;
    let listing = vis_of(&harness, &alice, "/photos").await;
    assert!(listing.contains("photos=private"), "{listing}");
    assert!(listing.contains("b.png=private"), "{listing}");

    // private: collections auto-created for an S3 style nested key
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/deep/deeper/c.md",
            Some(&alice),
            None,
            "x",
        ))
        .await;
    assert!(vis_of(&harness, &alice, "/deep").await.contains("deep=private"));
    assert!(vis_of(&harness, &alice, "/deep/deeper").await.contains("c.md=private"));

    // public: the shared drop box inherits `public`
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/public/shared.md",
            Some(&alice),
            None,
            "TOP SECRET PAYLOAD",
        ))
        .await;
    assert!(vis_of(&harness, &alice, "/public").await.contains("shared.md=public"));

    // explicit headers win over the inherited default, in both directions
    harness
        .send(harness
            .request_with_body(
                "PUT",
                "/dav/public/explicit-public.md",
                Some(&alice),
                None,
                "x",
            )
            .clone_header("x-meisync-visibility", "public"))
        .await;
    harness
        .send(harness
            .request_with_body(
                "PUT",
                "/dav/public/explicit-private.md",
                Some(&alice),
                None,
                "x",
            )
            .clone_header("x-meisync-visibility", "private"))
        .await;
    let listing = vis_of(&harness, &alice, "/public").await;
    assert!(listing.contains("explicit-public.md=public"), "{listing}");
    assert!(listing.contains("explicit-private.md=private"), "{listing}");

    // The namespace root refuses files: it only holds buckets (§30).
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/test.png",
            Some(&alice),
            None,
            "x",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // Consequence of the matrix: another account sees public nodes only.
    for (path, expected) in [
        ("/dav/public/shared.md", StatusCode::OK),
        ("/dav/public/explicit-public.md", StatusCode::OK),
        ("/dav/public/explicit-private.md", StatusCode::NOT_FOUND),
        ("/dav/photos/b.png", StatusCode::NOT_FOUND),
        ("/dav/deep/deeper/c.md", StatusCode::NOT_FOUND),
        ("/dav/private/alice/a.md", StatusCode::NOT_FOUND),
    ] {
        let response = harness
            .send(harness.request("GET", path, Some(&bob)))
            .await;
        assert_eq!(response.status(), expected, "{path}");
    }
}

#[tokio::test]
async fn logout_invalidates_the_token_immediately() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let response = harness
        .send(harness.request("GET", "/api/files/private/alice", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request("POST", "/api/auth/logout", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The very same token is now worthless on every protocol.
    for uri in [
        "/dav/private/alice",
        "/s3/alice",
        "/alice",
        "/api/files/private/alice",
    ] {
        let response = harness.send(harness.request("GET", uri, Some(&token))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn ysync_upgrade_requires_a_valid_token() {
    let (harness, _, _) = seeded().await;
    for query in ["", "?token=", "?token=bad", "?token=not-a-real-token"] {
        let uri = format!("/ysync/photos/test.png{query}");
        let response = harness.send(harness.request("GET", &uri, None)).await;
        assert!(
            response.status().is_client_error(),
            "{uri} -> {:?}",
            response.status()
        );
        assert!(!body_string(response).await.contains("TOP SECRET"));
    }
}