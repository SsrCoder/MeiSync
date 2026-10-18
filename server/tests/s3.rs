//! End-to-end S3 API coverage (`docs/mvp.md` §8, §25, §26, §30).

mod common;

use axum::http::StatusCode;
use common::{body_string, CloneHeader, Harness};
use hmac::Mac;
use sha2::Digest;

#[tokio::test]
async fn bucket_and_object_lifecycle() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // ListBuckets: every user sees their own bucket plus the shared one.
    let response = harness.send(harness.request("GET", "/s3", Some(&token))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let xml = body_string(response).await;
    assert!(xml.contains("<Name>alice</Name>"), "{xml}");
    assert!(xml.contains("<Name>public</Name>"), "{xml}");

    // PutObject
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/hello.txt",
            Some(&token),
            Some("text/plain"),
            "hello s3",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // GetObject returns exactly what was stored.
    let response = harness
        .send(harness.request("GET", "/s3/alice/hello.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/plain")
    );
    assert_eq!(body_string(response).await, "hello s3");

    // HeadObject
    let response = harness
        .send(harness.request("HEAD", "/s3/alice/hello.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-length").unwrap(), "8");

    // Nested keys create their intermediate collections implicitly.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/a/b/deep.txt",
            Some(&token),
            Some("text/plain"),
            "deep",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // ListObjectsV2 (flat)
    let response = harness
        .send(harness.request(
            "GET",
            "/s3/alice?list-type=2",
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let xml = body_string(response).await;
    assert!(xml.contains("<Key>hello.txt</Key>"), "{xml}");
    assert!(xml.contains("<Key>a/b/deep.txt</Key>"), "{xml}");
    assert!(xml.contains("<Size>4</Size>"), "{xml}");

    // ListObjectsV2 with a delimiter rolls sub trees up into CommonPrefixes.
    let response = harness
        .send(harness.request(
            "GET",
            "/s3/alice?list-type=2&delimiter=/",
            Some(&token),
        ))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<CommonPrefixes><Prefix>a/</Prefix></CommonPrefixes>"), "{xml}");
    assert!(xml.contains("<Key>hello.txt</Key>"), "{xml}");

    // ListObjectsV2 with a prefix
    let response = harness
        .send(harness.request(
            "GET",
            "/s3/alice?list-type=2&prefix=a/",
            Some(&token),
        ))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<Key>a/b/deep.txt</Key>"), "{xml}");
    assert!(!xml.contains("<Key>hello.txt</Key>"), "{xml}");

    // DeleteObject
    let response = harness
        .send(harness.request("DELETE", "/s3/alice/hello.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = harness
        .send(harness.request("GET", "/s3/alice/hello.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn s3_etag_is_shared_with_webdav() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/shared.md",
            Some(&token),
            Some("text/markdown"),
            "one ydoc, three protocols",
        ))
        .await;

    let s3_response = harness
        .send(harness.request("GET", "/s3/alice/shared.md", Some(&token)))
        .await;
    let s3_etag = s3_response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let dav_response = harness
        .send(harness.request("GET", "/dav/private/alice/shared.md", Some(&token)))
        .await;
    let dav_etag = dav_response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_eq!(s3_etag, dav_etag, "ETag is derived from the YDoc state");

    // A WebDAV write invalidates the S3 ETag immediately (§26).
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/shared.md",
            Some(&token),
            Some("text/markdown"),
            "changed through webdav",
        ))
        .await;
    let response = harness
        .send(harness.request("GET", "/s3/alice/shared.md", Some(&token)))
        .await;
    let new_etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_ne!(new_etag, s3_etag);
    assert_eq!(body_string(response).await, "changed through webdav");
}

#[tokio::test]
async fn s3_conditional_put() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/cond.txt",
            Some(&token),
            Some("text/plain"),
            "v1",
        ))
        .await;

    // Stale If-Match is rejected (§25).
    let request = harness
        .request_with_body(
            "PUT",
            "/s3/alice/cond.txt",
            Some(&token),
            Some("text/plain"),
            "v2",
        )
        .clone_header("if-match", "\"stale\"");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    // If-None-Match: * refuses to overwrite.
    let request = harness
        .request_with_body(
            "PUT",
            "/s3/alice/cond.txt",
            Some(&token),
            Some("text/plain"),
            "v2",
        )
        .clone_header("if-none-match", "*");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn create_bucket_and_use_it() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    let response = harness
        .send(harness.request("PUT", "/s3/projects", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request("HEAD", "/s3/projects", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/projects/readme.md",
            Some(&token),
            Some("text/markdown"),
            "# projects",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The bucket is a top level collection of the same namespace.
    let response = harness
        .send(harness.request("GET", "/dav/projects/readme.md", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "# projects");

    let response = harness
        .send(harness.request("PUT", "/s3/projects", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn webdav_collections_work_as_s3_buckets() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // A top level collection created over WebDAV is addressable as a bucket (§30).
    let response = harness
        .send(harness.request("MKCOL", "/dav/photos", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // It shows up in ListBuckets even though no bucket row was registered, while the system
    // owned `/private` namespace container does not.
    let response = harness.send(harness.request("GET", "/s3", Some(&token))).await;
    let xml = body_string(response).await;
    assert!(xml.contains("<Name>photos</Name>"), "{xml}");
    assert!(!xml.contains("<Name>private</Name>"), "{xml}");

    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/photos/2024/test.png",
            Some(&token),
            Some("image/png"),
            "\u{89}PNG\r\n\u{1a}\n",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // The very same bytes come back through S3.
    let response = harness
        .send(harness.request("GET", "/s3/photos/2024/test.png", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "\u{89}PNG\r\n\u{1a}\n");

    let response = harness
        .send(harness.request("GET", "/s3/photos?list-type=2", Some(&token)))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<Key>2024/test.png</Key>"), "{xml}");
}

#[tokio::test]
async fn root_level_files_are_refused() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // The namespace root only holds collections (buckets): a file there could never be
    // reached through the S3 API.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/test.png",
            Some(&token),
            Some("image/png"),
            "binary",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // Putting it into a collection works.
    let response = harness
        .send(harness.request("MKCOL", "/dav/photos", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/photos/test.png",
            Some(&token),
            Some("image/png"),
            "binary",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = harness
        .send(harness.request("GET", "/s3/photos/test.png", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn s3_is_also_served_from_the_endpoint_root() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/dav/private/alice/root-mount.txt",
            Some(&token),
            Some("text/plain"),
            "root mounted",
        ))
        .await;

    // `aws s3 --endpoint-url http://host:port` addresses the endpoint root, so the S3 API has
    // to be available there too (ListBuckets is `GET /`).
    let response = harness
        .send(harness.request("GET", "/alice/root-mount.txt", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "root mounted");

    let response = harness
        .send(harness.request("GET", "/alice?list-type=2&prefix=&delimiter=%2F", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let xml = body_string(response).await;
    assert!(xml.contains("<Key>root-mount.txt</Key>"), "{xml}");

    // Without AWS credentials the root still serves the human facing service index.
    let response = harness.send(harness.request("GET", "/", None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("\"service\":\"meisync\""), "{body}");
}

#[tokio::test]
async fn s3_requires_credentials_and_respects_permissions() {
    let harness = Harness::spawn().await;
    let alice = harness.register("alice").await;
    let bob = harness.register("bob").await;

    harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/notes.txt",
            Some(&alice),
            Some("text/plain"),
            "alice notes",
        ))
        .await;

    // No credentials at all.
    let response = harness.send(harness.request("GET", "/s3", None)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Bob may not read alice's bucket objects.
    let response = harness
        .send(harness.request("GET", "/s3/alice/notes.txt", Some(&bob)))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Bob may not overwrite them either.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/notes.txt",
            Some(&bob),
            Some("text/plain"),
            "hacked",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sigv4_authentication_works() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let username = "alice";

    // Sign the request exactly like an AWS SDK would.
    let method = "GET";
    let path = "/s3/alice";
    let query = "list-type=2";
    let host = "meisync.local";
    let amz_date = "20240102T030405Z";
    let date = "20240102";
    let payload_hash = "UNSIGNED-PAYLOAD";
    let signed = ["host", "x-amz-content-sha256", "x-amz-date"];

    let canonical_request = format!(
        "{method}\n{path}\n{query}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\n{}\n{payload_hash}",
        signed.join(";")
    );
    let scope = format!("{date}/us-east-1/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(sha256(canonical_request.as_bytes()))
    );
    let signature = hex::encode(hmac_sha256(
        &signing_key(token.as_bytes(), date, "us-east-1", "s3"),
        string_to_sign.as_bytes(),
    ));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={username}/{scope}, SignedHeaders={}, Signature={signature}",
        signed.join(";")
    );

    let request = harness
        .request("GET", "/s3/alice?list-type=2", None)
        .clone_header("host", host)
        .clone_header("x-amz-date", amz_date)
        .clone_header("x-amz-content-sha256", payload_hash)
        .clone_header("authorization", &authorization);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::OK, "SigV4 request accepted");

    // A wrong signature is rejected.
    let bad = authorization.replace(&signature, &"0".repeat(64));
    let request = harness
        .request("GET", "/s3/alice?list-type=2", None)
        .clone_header("host", host)
        .clone_header("x-amz-date", amz_date)
        .clone_header("x-amz-content-sha256", payload_hash)
        .clone_header("authorization", &bad);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// Build a SigV4 signed request, exactly like an AWS SDK would.
fn sigv4_request(
    harness: &Harness,
    method: &str,
    path_and_query: &str,
    access_key: &str,
    secret: &str,
) -> axum::http::Request<axum::body::Body> {
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, query),
        None => (path_and_query, ""),
    };
    let host = "meisync.local";
    let amz_date = "20240102T030405Z";
    let date = "20240102";
    let payload_hash = "UNSIGNED-PAYLOAD";
    let signed = ["host", "x-amz-content-sha256", "x-amz-date"];

    let canonical_request = format!(
        "{method}\n{path}\n{query}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\n{}\n{payload_hash}",
        signed.join(";")
    );
    let scope = format!("{date}/us-east-1/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(sha256(canonical_request.as_bytes()))
    );
    let signature = hex::encode(hmac_sha256(
        &signing_key(secret.as_bytes(), date, "us-east-1", "s3"),
        string_to_sign.as_bytes(),
    ));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={}, Signature={signature}",
        signed.join(";")
    );

    harness
        .request(method, path_and_query, None)
        .clone_header("host", host)
        .clone_header("x-amz-date", amz_date)
        .clone_header("x-amz-content-sha256", payload_hash)
        .clone_header("authorization", &authorization)
}

#[tokio::test]
async fn sigv4_rejects_the_account_name_as_the_secret_key() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // The access key is the *public* account name: signing with it as the secret must never
    // authenticate as that account (README: secret = any active session token).
    let response = harness
        .send(sigv4_request(&harness, "GET", "/s3/alice?list-type=2", "alice", "alice"))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The positive path is unchanged: a real session token works as the secret.
    let response = harness
        .send(sigv4_request(&harness, "GET", "/s3/alice?list-type=2", "alice", &token))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn control_characters_in_a_bucket_name_are_rejected() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;

    // `PUT /s3/x%0Ay` used to panic while building the `Location` header.
    let response = harness
        .send(harness.request("PUT", "/s3/x%0Ay", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // … and nothing was created.
    let response = harness.send(harness.request("GET", "/s3", Some(&token))).await;
    let xml = body_string(response).await;
    assert!(!xml.contains("<Name>x"), "{xml}");
}

#[tokio::test]
async fn get_object_honours_conditionals() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    harness
        .send(harness.request_with_body(
            "PUT",
            "/s3/alice/cond-get.txt",
            Some(&token),
            Some("text/plain"),
            "v1",
        ))
        .await;
    let response = harness
        .send(harness.request("GET", "/s3/alice/cond-get.txt", Some(&token)))
        .await;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_string();
    // No Range support: the header must not advertise ranges (§25).
    assert!(response.headers().get("accept-ranges").is_none());

    // If-None-Match with the current ETag → 304 (§25).
    let request = harness
        .request("GET", "/s3/alice/cond-get.txt", Some(&token))
        .clone_header("if-none-match", &etag);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // If-Match with a stale ETag → 412.
    let request = harness
        .request("GET", "/s3/alice/cond-get.txt", Some(&token))
        .clone_header("if-match", "\"stale\"");
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    // If-Match with the current ETag → 200.
    let request = harness
        .request("GET", "/s3/alice/cond-get.txt", Some(&token))
        .clone_header("if-match", &etag);
    let response = harness.send(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "v1");
}

#[tokio::test]
async fn list_objects_v2_reports_truncation_and_pages_with_the_token() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    for index in 0..5 {
        harness
            .send(harness.request_with_body(
                "PUT",
                &format!("/s3/alice/page-{index}.txt"),
                Some(&token),
                Some("text/plain"),
                "x",
            ))
            .await;
    }

    // First page: two keys, truncated, with a continuation token.
    let response = harness
        .send(harness.request(
            "GET",
            "/s3/alice?list-type=2&max-keys=2",
            Some(&token),
        ))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<IsTruncated>true</IsTruncated>"), "{xml}");
    assert!(xml.contains("<Key>page-0.txt</Key>"), "{xml}");
    assert!(xml.contains("<Key>page-1.txt</Key>"), "{xml}");
    assert!(!xml.contains("<Key>page-2.txt</Key>"), "{xml}");
    let tag = "<NextContinuationToken>";
    let token_start = xml.find(tag).expect("token") + tag.len();
    let token_end = xml[token_start..].find('<').expect("token end") + token_start;
    let continuation = xml[token_start..token_end].to_string();
    assert_eq!(continuation, "page-1.txt");

    // Second page starts after the token and is the last one.
    let response = harness
        .send(harness.request(
            "GET",
            &format!("/s3/alice?list-type=2&max-keys=2&continuation-token={continuation}"),
            Some(&token),
        ))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<IsTruncated>true</IsTruncated>"), "{xml}");
    assert!(xml.contains("<Key>page-2.txt</Key>"), "{xml}");
    assert!(xml.contains("<Key>page-3.txt</Key>"), "{xml}");
    assert!(!xml.contains("<Key>page-1.txt</Key>"), "{xml}");

    let response = harness
        .send(harness.request(
            "GET",
            "/s3/alice?list-type=2&max-keys=2&continuation-token=page-3.txt",
            Some(&token),
        ))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<IsTruncated>false</IsTruncated>"), "{xml}");
    assert!(xml.contains("<Key>page-4.txt</Key>"), "{xml}");
}

/// Objects and `CommonPrefixes` share one lexicographic sequence: paging a delimited listing
/// must neither drop nor duplicate anything, whichever of the two the page boundary hits.
#[tokio::test]
async fn list_objects_v2_pages_delimited_listings_without_dropping_keys() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    // Interleaved: `a/` < `b.txt` < `c/` < `d.txt`. With `max-keys=2` the old page boundary fell
    // between `b.txt` and `c/`, and `b.txt` was never returned again.
    for key in ["a/1", "b.txt", "c/1", "d.txt"] {
        let response = harness
            .send(harness.request_with_body(
                "PUT",
                &format!("/s3/alice/{key}"),
                Some(&token),
                Some("text/plain"),
                "x",
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK, "PUT {key}");
    }

    let mut entries: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut uri = "/s3/alice?list-type=2&delimiter=/&max-keys=2".to_string();
        if let Some(value) = &cursor {
            uri.push_str(&format!("&continuation-token={value}"));
        }
        let response = harness.send(harness.request("GET", &uri, Some(&token))).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let xml = body_string(response).await;
        entries.extend(listing_entries(&xml));
        pages += 1;
        assert!(pages <= 10, "paging must terminate: {entries:?}");
        if xml.contains("<IsTruncated>true</IsTruncated>") {
            cursor = Some(
                next_continuation_token(&xml)
                    .unwrap_or_else(|| panic!("a truncated page needs a token: {xml}")),
            );
        } else {
            assert!(
                !xml.contains("<NextContinuationToken>"),
                "an untruncated page carries no token: {xml}"
            );
            break;
        }
    }

    let mut sorted = entries.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        ["a/", "b.txt", "c/", "d.txt"],
        "the union of all pages is the full key set, exactly once"
    );

    // `max-keys=0` emits nothing, so it cannot be truncated and carries no token.
    let response = harness
        .send(harness.request("GET", "/s3/alice?list-type=2&delimiter=/&max-keys=0", Some(&token)))
        .await;
    let xml = body_string(response).await;
    assert!(xml.contains("<IsTruncated>false</IsTruncated>"), "{xml}");
    assert!(!xml.contains("<NextContinuationToken>"), "{xml}");
}

/// `PutObject` reports the revision the write produced, not the one it replaced.
#[tokio::test]
async fn put_object_reports_the_version_it_produced() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let uri = "/s3/alice/versioned.txt";

    let response = harness
        .send(harness.request_with_body("PUT", uri, Some(&token), Some("text/plain"), "v1"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let created = version_of(&response);
    let response = harness.send(harness.request("GET", uri, Some(&token))).await;
    assert_eq!(version_of(&response), created, "PUT and GET agree on the version");

    let response = harness
        .send(harness.request_with_body("PUT", uri, Some(&token), Some("text/plain"), "v2"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = version_of(&response);
    assert_eq!(updated, created + 1, "each write bumps the version exactly once");
    let response = harness
        .send(harness.request("GET", &format!("/api/files/private/alice/versioned.txt"), Some(&token)))
        .await;
    let meta: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("json");
    assert_eq!(meta["version"].as_i64(), Some(updated));
}

/// Object keys and `CommonPrefixes` roots of a `ListObjectsV2` page.
fn listing_entries(xml: &str) -> Vec<String> {
    let mut entries = Vec::new();
    for block in xml.split("<CommonPrefixes>").skip(1) {
        let block = block.split("</CommonPrefixes>").next().unwrap_or("");
        let prefix = block
            .split("<Prefix>")
            .nth(1)
            .unwrap_or("")
            .split("</Prefix>")
            .next()
            .unwrap_or("");
        entries.push(prefix.to_string());
    }
    for block in xml.split("<Contents>").skip(1) {
        let block = block.split("</Contents>").next().unwrap_or("");
        let key = block
            .split("<Key>")
            .nth(1)
            .unwrap_or("")
            .split("</Key>")
            .next()
            .unwrap_or("");
        entries.push(key.to_string());
    }
    entries
}

fn next_continuation_token(xml: &str) -> Option<String> {
    let tag = "<NextContinuationToken>";
    let start = xml.find(tag)? + tag.len();
    let end = xml[start..].find('<')? + start;
    Some(xml[start..end].to_string())
}

fn version_of(response: &axum::http::Response<axum::body::Body>) -> i64 {
    response
        .headers()
        .get("x-meisync-version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("missing x-meisync-version"))
        .parse()
        .expect("numeric version")
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("hmac");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn signing_key(secret: &[u8], date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{}", String::from_utf8_lossy(secret)).as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}
