//! WebDAV frontend (`docs/mvp.md` §7).
//!
//! Supported methods: `GET`, `HEAD`, `PUT`, `DELETE`, `PROPFIND`, `MKCOL`, `MOVE`, `COPY`.
//!
//! All of them are *views* over the same FileDocument: `GET` materialises the YDoc, `PUT`
//! replaces the YDoc content inside a transaction – nothing is ever written to the filesystem
//! behind the YDoc's back.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
pub mod util;

use axum::routing::any;
use axum::Router;
use meisync_api::service::{CreateFileSpec, Depth, FileService};
use meisync_api::AppState;
use meisync_core::{FileKind, FileMetadata, MeiSyncError, Preconditions, Visibility};
use std::sync::Arc;

use crate::util::{escape, format_millis, if_match_allows, if_none_match_matches, normalize_etag};

/// Where the WebDAV endpoints are mounted, needed to parse `Destination:` headers.
pub const DAV_MOUNT: &str = "/dav";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dav", any(handle))
        .route("/dav/", any(handle))
        .route("/dav/{*path}", any(handle))
}

/// Convenience mount that also serves the namespace from `/`
/// (handy for `davfs2` / `rclone webdav` without a prefix).
pub fn router_at_root() -> Router<AppState> {
    Router::new()
        .route("/", any(handle))
        .route("/{*path}", any(handle))
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    dispatch(Arc::new(state), DAV_MOUNT.to_string(), parts, body).await
}

async fn dispatch(
    state: Arc<AppState>,
    mount: String,
    parts: axum::http::request::Parts,
    body: Body,
) -> Response {
    let method = parts.method.clone();
    let headers = parts.headers.clone();
    let uri: Uri = parts.uri.clone();
    let user = match meisync_api::extract::AuthToken::from_parts(&parts)
        .or_else(|| meisync_api::extract::AuthToken::from_query(&parts))
    {
        Some(token) => match state.auth.authenticate(&token).await {
            Ok(user) => user,
            Err(err) => return error_response(&err),
        },
        None => {
            return error_response(&MeiSyncError::Unauthorized(
                "WebDAV requires a bearer token".into(),
            ))
        }
    };

    let path = request_path(uri.path(), &mount);

    let result = match method.as_str() {
        "OPTIONS" => options_response(),
        "GET" => get_or_head(&state, &user, &path, &headers, false).await,
        "HEAD" => get_or_head(&state, &user, &path, &headers, true).await,
        "PUT" => put(&state, &user, &path, &headers, body).await,
        "DELETE" => delete(&state, &user, &path).await,
        "PROPFIND" => propfind(&state, &user, &path, &headers, body).await,
        "MKCOL" => mkcol(&state, &user, &path, &headers).await,
        "MOVE" => move_or_copy(&state, &user, &path, &headers, true).await,
        "COPY" => move_or_copy(&state, &user, &path, &headers, false).await,
        "LOCK" | "UNLINK" | "UNLOCK" => Err(MeiSyncError::NotImplemented(format!(
            "{method} is not supported"
        ))),
        other => Err(MeiSyncError::NotImplemented(format!(
            "{other} is not supported by the WebDAV frontend"
        ))),
    };

    match result {
        Ok(response) => response,
        Err(err) => error_response(&err),
    }
}

/// Namespace path of the request, relative to the WebDAV mount point.
pub fn request_path(uri_path: &str, mount: &str) -> String {
    let mut path = uri_path.to_string();
    let mount = mount.trim_end_matches('/');
    if !mount.is_empty() {
        if let Some(stripped) = path.strip_prefix(mount) {
            path = stripped.to_string();
        }
    }
    if path.is_empty() {
        path = "/".to_string();
    }
    // Percent decoding for names containing spaces or unicode.
    let decoded = percent_decode(&path);
    if decoded.starts_with('/') {
        decoded
    } else {
        format!("/{decoded}")
    }
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

// ---------------------------------------------------------------------------------------------
// Method implementations
// ---------------------------------------------------------------------------------------------

fn options_response() -> Result<Response, MeiSyncError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        HeaderName::from_static("dav"),
        HeaderValue::from_static("1, 2, 3"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("allow"),
        HeaderValue::from_static(
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, MOVE, COPY, LOCK, UNLOCK",
        ),
    );
    response.headers_mut().insert(
        HeaderName::from_static("ms-author-via"),
        HeaderValue::from_static("DAV"),
    );
    Ok(response)
}

async fn get_or_head(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
    headers: &HeaderMap,
    head_only: bool,
) -> Result<Response, MeiSyncError> {
    let meta = state.files.get(user, path).await?;
    if meta.is_collection() {
        return Err(MeiSyncError::Conflict("path is a collection".into()));
    }
    // ETag is checked before materialising the content (§25, §26).
    let etag = meta.etag.clone();
    if !if_match_allows(headers, etag.as_deref()) {
        return Err(MeiSyncError::PreconditionFailed(format!(
            "If-Match failed for {path}"
        )));
    }
    if if_none_match_matches(headers, etag.as_deref()) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        if let Some(etag) = etag {
            if let Ok(value) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, value);
            }
        }
        return Ok(response);
    }

    let snapshot = state.files.read_content(user, path).await?;
    let etag = snapshot.etag.clone();
    let mut response = Response::new(Body::from(snapshot.content));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format_millis(meta.updated_at.timestamp_millis())) {
        headers.insert(header::LAST_MODIFIED, value);
    }
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(snapshot.size),
    );
    headers.insert(
        HeaderName::from_static("x-meisync-version"),
        HeaderValue::from(snapshot.version),
    );
    if head_only {
        *response.body_mut() = Body::empty();
    }
    Ok(response)
}

async fn put(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, MeiSyncError> {
    // Only used for the status code; the authoritative precondition check happens inside the
    // document lock (§25).
    let existed = state.files.get(user, path).await.is_ok();
    let preconditions = preconditions_of(headers);

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let body = axum::body::to_bytes(body, state.config.max_body_bytes)
        .await
        .map_err(|err| MeiSyncError::BadRequest(err.to_string()))?
        .to_vec();

    let snapshot = state
        .files
        .replace_content_checked(user, path, body, content_type, preconditions)
        .await?;

    if let Some(visibility) = visibility_from_headers(headers) {
        if let Ok(meta) = state.files.get(user, path).await {
            let _ = state.files.set_visibility(user, &meta.id, visibility).await;
        }
    }

    let mut response = Response::new(Body::empty());
    *response.status_mut() = if existed {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CREATED
    };
    if let Ok(value) = HeaderValue::from_str(&snapshot.etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response.headers_mut().insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&format_millis(chrono::Utc::now().timestamp_millis())).unwrap(),
    );
    if !existed {
        if let Ok(meta) = state.files.get(user, path).await {
            let location = format!("/private/{}", user.username);
            let _ = meta;
            if let Ok(value) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::CONTENT_LOCATION, value);
            }
        }
    }
    Ok(response)
}

async fn delete(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
) -> Result<Response, MeiSyncError> {
    state.files.delete(user, path).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    Ok(response)
}

async fn mkcol(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
    headers: &HeaderMap,
) -> Result<Response, MeiSyncError> {
    let _ = headers;
    let (parent_path, name) =
        meisync_api::service::DefaultFileService::split_parent(path)?;
    // RFC 4918 §8.3.1: MKCOL on an existing resource is "Method Not Allowed".
    if state.files.get(user, path).await.is_ok() {
        return Err(MeiSyncError::MethodNotAllowed(format!(
            "{path} already exists"
        )));
    }
    // RFC 4918: a missing intermediate collection is a conflict, not a plain 404. The parent is
    // resolved separately so that a *hidden* parent is not reported as a missing one.
    if let Err(MeiSyncError::NotFound(_)) = state
        .files
        .resolve_existing_parent(user, &parent_path)
        .await
    {
        return Err(MeiSyncError::Conflict(format!(
            "parent collection of {path} does not exist"
        )));
    }

    let visibility = visibility_from_headers(headers);
    let created = state
        .files
        .create(
            user,
            CreateFileSpec {
                parent_path,
                name,
                kind: FileKind::Collection,
                content_type: None,
                visibility,
                initial_content: None,
            },
        )
        .await;
    let _created = match created {
        Ok(meta) => meta,
        // A `NotFound` here means the parent collection is not visible for the caller, which
        // must stay a `404` (README: private files of other users answer 404, never 403).
        Err(err) => return Err(err),
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn move_or_copy(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
    headers: &HeaderMap,
    is_move: bool,
) -> Result<Response, MeiSyncError> {
    let destination = headers
        .get("destination")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| MeiSyncError::bad_request("missing Destination header"))?;
    let destination = destination_path(destination);
    let overwrite = headers
        .get("overwrite")
        .and_then(|v| v.to_str().ok())
        .map(|v| !v.eq_ignore_ascii_case("F"))
        .unwrap_or(true);

    let (dest_parent, dest_name) =
        meisync_api::service::DefaultFileService::split_parent(&destination)?;

    let existed = state.files.get(user, &destination).await.is_ok();

    if is_move {
        state
            .files
            .move_file(user, path, &dest_parent, Some(&dest_name), overwrite)
            .await?;
    } else {
        // COPY is MOVE followed by a full materialisation of the source into the target. The
        // source content type decides the encoding: letting the destination *name* pick it
        // would re-encode (and corrupt) binary content copied to a text extension.
        let source = state.files.get(user, path).await?;
        let content = state.files.read_content(user, path).await?;
        let target = meisync_api::service::DefaultFileService::join_path(&dest_parent, &dest_name);
        state
            .files
            .replace_content(user, &target, content.content, source.content_type)
            .await?;
    }

    let mut response = Response::new(Body::empty());
    *response.status_mut() = if existed {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CREATED
    };
    Ok(response)
}

/// `Destination` may be an absolute URL or an absolute path.
pub fn destination_path(destination: &str) -> String {
    if let Ok(uri) = destination.parse::<Uri>() {
        request_path(uri.path(), DAV_MOUNT)
    } else {
        // Strip a mount prefix if present.
        let path = destination
            .strip_prefix("http://")
            .or_else(|| destination.strip_prefix("https://"))
            .unwrap_or(destination);
        match path.find('/') {
            Some(idx) => request_path(&path[idx..], DAV_MOUNT),
            None => request_path(path, DAV_MOUNT),
        }
    }
}

fn visibility_from_headers(headers: &HeaderMap) -> Option<Visibility> {
    headers
        .get("x-meisync-visibility")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            _ => None,
        })
}

/// `If-Match` / `If-None-Match` of a write request, evaluated inside the document lock.
fn preconditions_of(headers: &HeaderMap) -> Preconditions {
    Preconditions::new(
        headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()),
        headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok()),
    )
}

// ---------------------------------------------------------------------------------------------
// PROPFIND
// ---------------------------------------------------------------------------------------------

async fn propfind(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, MeiSyncError> {
    let depth = Depth::parse(
        headers
            .get("depth")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1"),
    );
    let body = axum::body::to_bytes(body, 1024 * 1024)
        .await
        .map_err(|err| MeiSyncError::BadRequest(err.to_string()))?;
    let request_body = String::from_utf8_lossy(&body).to_ascii_lowercase();
    let propname_only = request_body.contains("propname");

    let meta = state.files.get(user, path).await?;
    let mut responses = vec![response_xml(&meta, state, propname_only).await?];
    if depth != Depth::Zero && meta.is_collection() {
        for child in state.files.list(user, path, depth).await? {
            responses.push(response_xml(&child, state, propname_only).await?);
        }
    }

    let payload = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">{}</D:multistatus>",
        responses.join("")
    );
    let mut response = Response::new(Body::from(payload));
    *response.status_mut() = StatusCode::MULTI_STATUS;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("dav"),
        HeaderValue::from_static("1, 2, 3"),
    );
    Ok(response)
}

async fn response_xml(
    meta: &FileMetadata,
    state: &AppState,
    propname_only: bool,
) -> Result<String, MeiSyncError> {
    let path = state.files.path_of(&meta.id).await?;
    let href = format!("{DAV_MOUNT}{}", encode_href(&path));
    let resourcetype = if meta.is_collection() {
        "<D:collection/>"
    } else {
        ""
    };
    let value = |inner: &str| -> String {
        if propname_only {
            String::new()
        } else {
            inner.to_string()
        }
    };
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| meisync_core::guess_content_type(&meta.name));
    let etag = meta
        .etag
        .clone()
        .unwrap_or_else(|| "\"0\"".to_string());
    let size = if meta.is_collection() { 0 } else { meta.size };
    let last_modified = format_millis(meta.updated_at.timestamp_millis());
    let created = format_millis(meta.created_at.timestamp_millis());

    Ok(format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>\
         <D:displayname>{name}</D:displayname>\
         <D:resourcetype>{resourcetype}</D:resourcetype>\
         <D:getcontentlength>{size}</D:getcontentlength>\
         <D:getcontenttype>{ct}</D:getcontenttype>\
         <D:getetag>{etag}</D:getetag>\
         <D:getlastmodified>{modified}</D:getlastmodified>\
         <D:creationdate>{created}</D:creationdate>\
         <D:supportedlock/>\
         </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        escape(&href),
        name = escape(&meta.name),
        resourcetype = resourcetype,
        size = value(&size.to_string()),
        ct = value(&escape(&content_type)),
        etag = value(&escape(&etag)),
        modified = value(&last_modified),
        created = value(&created),
    ))
}

fn encode_href(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<String>>()
        .join("/")
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// WebDAV errors are plain text with a matching status code.
pub fn error_response(err: &MeiSyncError) -> Response {
    let status = meisync_api::error::status_of(err);
    let mut response = Response::new(Body::from(format!("{err}\n")));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let MeiSyncError::Forbidden(_) = err {
        response.headers_mut().insert(
            HeaderName::from_static("x-meisync-error"),
            HeaderValue::from_static("forbidden"),
        );
    }
    response
}

/// ETag of the current state, used by clients that poll (`If-None-Match`).
pub async fn current_etag(
    state: &AppState,
    user: &meisync_core::User,
    path: &str,
) -> Option<String> {
    state
        .files
        .get(user, path)
        .await
        .ok()
        .map(|meta| meta.etag.unwrap_or_else(|| normalize_etag("\"0\"")))
}