//! S3-compatible API (`docs/mvp.md` §8, §30).
//!
//! ```text
//! GET    /s3                                  ListBuckets
//! PUT    /s3/{bucket}                         CreateBucket
//! GET    /s3/{bucket}?list-type=2              ListObjectsV2
//! POST   /s3/{bucket}?delete                  DeleteObjects
//! GET    /s3/{bucket}/{key}                   GetObject
//! HEAD   /s3/{bucket}/{key}                   HeadObject
//! PUT    /s3/{bucket}/{key}                   PutObject
//! DELETE /s3/{bucket}/{key}                   DeleteObject
//! ```
//!
//! A bucket is a top level collection of the file namespace (`bucket/key` = collection path),
//! so the S3 API resolves keys through the very same `FileService` as WebDAV.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use meisync_api::service::{CreateFileSpec, FileService, RecursiveListSpec};
use meisync_api::AppState;
use meisync_core::{FileKind, MeiSyncError, Preconditions, User, Visibility};
use std::collections::{BTreeMap, HashMap};

pub mod sigv4;
mod xml;

use xml::{escape, write_object_entry, ObjectEntry};

pub const S3_MOUNT: &str = "/s3";
const XML_CONTENT_TYPE: &str = "application/xml";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/s3", any(handle))
        .route("/s3/", any(handle))
        .route("/s3/{*path}", any(handle))
}

/// Serve the S3 API from the endpoint root, the way stock S3 clients expect it
/// (`--endpoint-url http://host:port` ⇒ `GET /` = ListBuckets, `GET /<bucket>/<key>` = object).
pub fn router_at_root() -> Router<AppState> {
    Router::new().route("/{*path}", any(handle))
}

/// Is this request an S3 API call (AWS SigV4 signed) rather than a human facing request?
pub fn is_sigv4(parts: &axum::http::request::Parts) -> bool {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("AWS4-HMAC-SHA256"))
        .unwrap_or(false)
}

/// Entry point for handlers that delegate to the S3 API.
pub async fn serve(state: AppState, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let state = std::sync::Arc::new(state);
    dispatch(state, parts, body).await
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let state = std::sync::Arc::new(state);
    let response = dispatch(state, parts, body).await;
    response
}

async fn dispatch(
    state: std::sync::Arc<AppState>,
    parts: axum::http::request::Parts,
    body: Body,
) -> Response {
    let method = parts.method.as_str().to_string();
    let headers: BTreeMap<String, String> = parts
        .headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_ascii_lowercase(), v.to_string())))
        .collect();
    let uri: Uri = parts.uri.clone();

    let user = match authenticate(&state, &parts, &headers, &uri).await {
        Ok(user) => user,
        Err(err) => return xml_error(&err),
    };

    let body_bytes = match axum::body::to_bytes(body, state.config.max_body_bytes).await {
        Ok(bytes) => bytes.to_vec(),
        Err(err) => return xml_error(&MeiSyncError::BadRequest(err.to_string())),
    };

    let query = parse_query(uri.query());
    let (bucket, key) = split_path(uri.path(), S3_MOUNT);

    let result = match method.as_str() {
        "GET" if bucket.is_none() => list_buckets(&state, &user).await,
        "GET" if key.as_deref().unwrap_or("").is_empty() => {
            list_objects(&state, &user, bucket.as_deref().unwrap_or(""), &query).await
        }
        "GET" => {
            get_object(
                &state,
                &user,
                bucket.as_deref().unwrap_or(""),
                key.as_deref().unwrap_or(""),
                &headers,
            )
            .await
        }
        "HEAD" if key.as_deref().unwrap_or("").is_empty() => head_bucket(&state, &user, bucket.as_deref().unwrap_or("")).await,
        "HEAD" => {
            get_object(
                &state,
                &user,
                bucket.as_deref().unwrap_or(""),
                key.as_deref().unwrap_or(""),
                &headers,
            )
            .await
        }
        "PUT" if key.as_deref().unwrap_or("").is_empty() => {
            create_bucket(&state, &user, bucket.as_deref().unwrap_or("")).await
        }
        "PUT" => put_object(
            &state,
            &user,
            bucket.as_deref().unwrap_or(""),
            key.as_deref().unwrap_or(""),
            &headers,
            body_bytes,
        )
        .await,
        "DELETE" if key.as_deref().unwrap_or("").is_empty() => Err(MeiSyncError::BadRequest(
            "DeleteBucket is not supported; delete the collection through WebDAV".into(),
        )),
        "DELETE" => delete_object(
            &state,
            &user,
            bucket.as_deref().unwrap_or(""),
            key.as_deref().unwrap_or(""),
        )
        .await,
        "POST" => delete_objects(&state, &user, bucket.as_deref().unwrap_or(""), &body_bytes).await,
        other => Err(MeiSyncError::NotImplemented(format!(
            "S3 method {other} is not supported"
        ))),
    };

    match result {
        Ok(response) => response,
        Err(err) => xml_error(&err),
    }
}

// ---------------------------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------------------------

/// Bearer token or AWS SigV4 signature.
///
/// SigV4 credentials are `(access key, secret key)`:
/// * `access key` – the account name, or a session token
/// * `secret key` – one of the session tokens issued for that account
async fn authenticate(
    state: &AppState,
    parts: &axum::http::request::Parts,
    headers: &BTreeMap<String, String>,
    uri: &Uri,
) -> Result<User, MeiSyncError> {
    // 1. Simple bearer token (used by curl / custom clients).
    if let Some(token) = meisync_api::extract::AuthToken::from_parts(parts)
        .or_else(|| meisync_api::extract::AuthToken::from_query(parts))
    {
        if let Ok(user) = state.auth.authenticate(&token).await {
            return Ok(user);
        }
    }

    // 2. AWS SigV4.
    let authorization = headers
        .get("authorization")
        .cloned()
        .ok_or_else(|| MeiSyncError::Unauthorized("missing credentials".into()))?;
    if !authorization.starts_with("AWS4-HMAC-SHA256") {
        return Err(MeiSyncError::Unauthorized(
            "unsupported authorization scheme".into(),
        ));
    }
    let credentials = sigv4::parse_authorization(&authorization)
        .map_err(|err| MeiSyncError::Unauthorized(err.to_string()))?;

    // Resolve the account behind the access key.
    let (user, access_key_is_session) = match state
        .auth
        .users()
        .find_by_username(&credentials.access_key)
        .await?
    {
        Some(user) => (user, false),
        // The access key *is* a session token, which is also accepted.
        None => (state.auth.authenticate(&credentials.access_key).await?, true),
    };

    let request = sigv4::SigV4Request {
        method: parts.method.as_str(),
        path: uri.path(),
        query: uri.query(),
        headers,
        body: &[],
    };

    // Every issued session token of that account is a valid secret key. The access key itself
    // is *never* a secret: it is the public account name, and signing with it must not
    // authenticate as that account (§11).
    let mut secrets: Vec<String> = state
        .auth
        .sessions()
        .list_for_user(&user.id)
        .await?
        .into_iter()
        .map(|session| session.token)
        .collect();
    if access_key_is_session {
        secrets.push(credentials.access_key.clone());
    }

    for secret in secrets {
        if sigv4::verify(&request, &secret).is_ok() {
            return Ok(user);
        }
    }
    Err(MeiSyncError::Forbidden(
        "S3 signature does not match any active session token".into(),
    ))
}

// ---------------------------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------------------------

pub fn split_path(path: &str, mount: &str) -> (Option<String>, Option<String>) {
    let trimmed = path.strip_prefix(mount).unwrap_or(path);
    let rest = trimmed.trim_start_matches('/');
    if rest.is_empty() {
        return (None, None);
    }
    match rest.split_once('/') {
        Some((bucket, key)) => (
            Some(decode(bucket)),
            Some(decode(key)).filter(|k| !k.is_empty()),
        ),
        None => (Some(decode(rest)), None),
    }
}

fn decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .to_string()
}

fn parse_query(query: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(query) = query {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            out.insert(
                decode(key).to_ascii_lowercase(),
                decode(value),
            );
        }
    }
    out
}

fn key_path(bucket_collection_path: &str, key: &str) -> String {
    format!("{}/{key}", bucket_collection_path.trim_end_matches('/'))
}

// ---------------------------------------------------------------------------------------------
// Bucket operations
// ---------------------------------------------------------------------------------------------

async fn list_buckets(state: &AppState, user: &User) -> Result<Response, MeiSyncError> {
    let buckets = state.files.buckets().await?;
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    body.push_str("<Owner><ID>");
    body.push_str(&escape(&user.id.0));
    body.push_str("</ID><DisplayName>");
    body.push_str(&escape(&user.username));
    body.push_str("</DisplayName></Owner><Buckets>");
    for (name, _id, owner) in buckets {
        let meta = state.files.bucket_collection(&name).await.ok();
        let readable = match meta {
            Some(meta) => state
                .files
                .effective_permissions(user, &meta)
                .await
                .map(|p| p.has(meisync_core::PermissionSet::READ))
                .unwrap_or(false),
            None => false,
        };
        if !readable {
            continue;
        }
        body.push_str("<Bucket><Name>");
        body.push_str(&escape(&name));
        body.push_str("</Name><CreationDate>");
        body.push_str(&xml::iso8601(chrono::Utc::now()));
        body.push_str("</CreationDate>");
        body.push_str("<Owner><ID>");
        body.push_str(&escape(&owner.unwrap_or_default()));
        body.push_str("</ID></Owner></Bucket>");
    }
    body.push_str("</Buckets></ListAllMyBucketsResult>");
    Ok(xml_response(StatusCode::OK, body))
}

async fn head_bucket(
    state: &AppState,
    user: &User,
    bucket: &str,
) -> Result<Response, MeiSyncError> {
    let meta = state.files.bucket_collection(bucket).await?;
    state.files.get_by_id(user, &meta.id).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(HeaderName::from_static("x-amz-bucket-region"), HeaderValue::from_static("meisync"));
    Ok(response)
}

async fn create_bucket(
    state: &AppState,
    user: &User,
    bucket: &str,
) -> Result<Response, MeiSyncError> {
    if state.files.bucket_collection(bucket).await.is_ok() {
        return Err(MeiSyncError::Conflict(format!(
            "bucket already exists: {bucket}"
        )));
    }
    let meta = state
        .files
        .create(
            user,
            CreateFileSpec {
                parent_path: "/".to_string(),
                name: bucket.to_string(),
                kind: FileKind::Collection,
                content_type: None,
                visibility: Some(Visibility::Private),
                initial_content: None,
            },
        )
        .await?;
    state.files.register_bucket(bucket, &meta.id, &user.id).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/{bucket}")).unwrap(),
    );
    Ok(response)
}

async fn list_objects(
    state: &AppState,
    user: &User,
    bucket: &str,
    query: &HashMap<String, String>,
) -> Result<Response, MeiSyncError> {
    let root = state.files.bucket_collection(bucket).await?;
    let base = state.files.path_of(&root.id).await?;
    let prefix = query.get("prefix").cloned().unwrap_or_default();
    let delimiter = query.get("delimiter").cloned().filter(|d| !d.is_empty());
    let max_keys: usize = query
        .get("max-keys")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    // The continuation token is the last element of the previous page's merged object /
    // `CommonPrefixes` sequence: the next page resumes strictly after it.
    let continuation = query
        .get("continuation-token")
        .filter(|token| !token.is_empty())
        .cloned();

    let listing = state
        .files
        .list_recursive(
            user,
            &RecursiveListSpec {
                root: &root,
                base_path: &base,
                prefix: &prefix,
                delimiter: delimiter.as_deref(),
                start_after: continuation.as_deref(),
                max_keys,
            },
        )
        .await?;

    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    body.push_str(&format!("<Name>{}</Name>", escape(bucket)));
    body.push_str(&format!("<Prefix>{}</Prefix>", escape(&prefix)));
    body.push_str(&format!("<MaxKeys>{max_keys}</MaxKeys>"));
    body.push_str(if listing.truncated {
        "<IsTruncated>true</IsTruncated>"
    } else {
        "<IsTruncated>false</IsTruncated>"
    });
    if let Some(delimiter) = &delimiter {
        body.push_str(&format!("<Delimiter>{}</Delimiter>", escape(delimiter)));
    }
    for common in &listing.prefixes {
        body.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            escape(common)
        ));
    }
    for meta in &listing.objects {
        let path = state.files.path_of(&meta.id).await?;
        let key = path
            .strip_prefix(&base)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .unwrap_or_default();
        let content_type = meta
            .content_type
            .clone()
            .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));
        let entry = ObjectEntry {
            key: &key,
            size: meta.size,
            etag: meta.etag.clone().unwrap_or_else(|| "\"0\"".into()),
            last_modified: meta.updated_at,
            storage_class: "STANDARD",
        };
        write_object_entry(&mut body, &entry, &content_type);
    }
    if let Some(token) = &listing.next_continuation_token {
        body.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            escape(token)
        ));
    }
    body.push_str("</ListBucketResult>");
    Ok(xml_response(StatusCode::OK, body))
}

// ---------------------------------------------------------------------------------------------
// Object operations
// ---------------------------------------------------------------------------------------------

async fn get_object(
    state: &AppState,
    user: &User,
    bucket: &str,
    key: &str,
    request_headers: &BTreeMap<String, String>,
) -> Result<Response, MeiSyncError> {
    let root = state.files.bucket_collection(bucket).await?;
    let base = state.files.path_of(&root.id).await?;
    let path = key_path(&base, key);
    let meta = state.files.get(user, &path).await?;

    // Conditional GET, mirroring WebDAV (§25): the ETag lives in the metadata, so it is stable
    // for the whole read.
    let preconditions = Preconditions::new(
        request_headers.get("if-match").map(String::as_str),
        request_headers.get("if-none-match").map(String::as_str),
    );
    if !preconditions.if_match_holds(meta.etag.as_deref()) {
        return Err(MeiSyncError::PreconditionFailed(format!(
            "If-Match failed for {path}"
        )));
    }
    if preconditions.none_match_matches(meta.etag.as_deref()) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        if let Some(etag) = meta.etag.as_deref() {
            if let Ok(value) = HeaderValue::from_str(etag) {
                response.headers_mut().insert(header::ETAG, value);
            }
        }
        return Ok(response);
    }

    let snapshot = state.files.read_content(user, &path).await?;
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));

    let mut response = Response::new(Body::from(snapshot.content));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(snapshot.size),
    );
    if let Ok(value) = HeaderValue::from_str(&snapshot.etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        HeaderName::from_static("last-modified"),
        HeaderValue::from_str(&xml::http_date(meta.updated_at))
            .unwrap_or(HeaderValue::from_static("")),
    );
    headers.insert(
        HeaderName::from_static("x-meisync-version"),
        HeaderValue::from(snapshot.version),
    );
    Ok(response)
}

async fn put_object(
    state: &AppState,
    user: &User,
    bucket: &str,
    key: &str,
    headers: &BTreeMap<String, String>,
    body: Vec<u8>,
) -> Result<Response, MeiSyncError> {
    let root = state.files.bucket_collection(bucket).await?;
    let base = state.files.path_of(&root.id).await?;
    let path = key_path(&base, key);
    let content_type = headers.get("content-type").cloned();

    // Optimistic concurrency, mirroring WebDAV (§25). The preconditions are evaluated inside
    // the document lock, so two concurrent conditional PUTs cannot both succeed.
    let preconditions = Preconditions::new(
        headers.get("if-match").map(String::as_str),
        headers.get("if-none-match").map(String::as_str),
    );

    let snapshot = state
        .files
        .replace_content_checked(user, &path, body, content_type, preconditions)
        .await?;

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&snapshot.etag).unwrap());
    response
        .headers_mut()
        .insert(HeaderName::from_static("x-meisync-version"), HeaderValue::from(snapshot.version));
    Ok(response)
}

async fn delete_object(
    state: &AppState,
    user: &User,
    bucket: &str,
    key: &str,
) -> Result<Response, MeiSyncError> {
    let root = state.files.bucket_collection(bucket).await?;
    let base = state.files.path_of(&root.id).await?;
    let path = key_path(&base, key);
    state.files.delete(user, &path).await?;
    // S3 answers 204 even when the object did not exist.
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    Ok(response)
}

/// `POST /s3/{bucket}?delete` with an XML `Delete` document.
async fn delete_objects(
    state: &AppState,
    user: &User,
    bucket: &str,
    body: &[u8],
) -> Result<Response, MeiSyncError> {
    let text = String::from_utf8_lossy(body);
    let keys: Vec<String> = text
        .split("<Key>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</Key>").next())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect();
    if keys.is_empty() {
        return Err(MeiSyncError::BadRequest("no keys in Delete request".into()));
    }
    let mut result_body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for key in keys {
        let outcome = delete_object(state, user, bucket, &key).await;
        result_body.push_str("<Deleted><Key>");
        result_body.push_str(&escape(&key));
        result_body.push_str("</Key>");
        if outcome.is_err() {
            // S3 reports missing keys as deleted; surface real errors instead.
            result_body.push_str("<Code>InternalError</Code><Message>");
            result_body.push_str(&escape("delete failed"));
            result_body.push_str("</Message>");
        }
        result_body.push_str("</Deleted>");
    }
    result_body.push_str("</DeleteResult>");
    Ok(xml_response(StatusCode::OK, result_body))
}

// ---------------------------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------------------------

pub fn xml_response(status: StatusCode, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(XML_CONTENT_TYPE));
    response
}

/// S3 style XML error document.
pub fn xml_error(err: &MeiSyncError) -> Response {
    let (status, code, message) = match err {
        MeiSyncError::NotFound(msg) => (StatusCode::NOT_FOUND, "NoSuchKey", msg.clone()),
        MeiSyncError::Forbidden(msg) => (StatusCode::FORBIDDEN, "AccessDenied", msg.clone()),
        MeiSyncError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "InvalidAccessKeyId", msg.clone()),
        MeiSyncError::Conflict(msg) => (StatusCode::CONFLICT, "BucketAlreadyExists", msg.clone()),
        MeiSyncError::AlreadyExists(msg) => (StatusCode::CONFLICT, "EntityAlreadyExists", msg.clone()),
        MeiSyncError::PreconditionFailed(msg) => {
            (StatusCode::PRECONDITION_FAILED, "PreconditionFailed", msg.clone())
        }
        MeiSyncError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "InvalidRequest", msg.clone()),
        MeiSyncError::NotImplemented(msg) => {
            (StatusCode::NOT_IMPLEMENTED, "NotImplemented", msg.clone())
        }
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            other.to_string(),
        ),
    };
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{}</Message><Resource>{S3_MOUNT}</Resource></Error>",
        escape(&message)
    );
    let mut response = xml_response(status, body);
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("AWS4-HMAC-SHA256"),
        );
    }
    response
}

/// Convenience for tests / clients: build a header map from pairs.
pub fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}