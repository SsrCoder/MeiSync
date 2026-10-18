//! Wiring of a MeiSync node: HTTP router + application state.
//!
//! ```text
//! /api/...   JSON control plane (auth, file info, ACL)
//! /dav/...   WebDAV          (§7)
//! /s3/...    S3 compatible   (§8, §30)
//! /ysync/... Yjs/Yrs sync    (§9, §10)
//! ```

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use meisync_api::AppState;
use meisync_document::DocumentManagerConfig;
use meisync_persistence::fs_store::CompactionPolicy;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

pub use meisync_api::ServerConfig;

/// Build the full HTTP router for a booted node.
pub fn router(state: std::sync::Arc<AppState>) -> Router {
    let max_body = state.config.max_body_bytes;

    let mut app = Router::new()
        .route("/", get(root))
        .route("/health", get(|| async { "ok" }))
        .nest("/api", meisync_api::routes::router())
        .nest("/api/admin", meisync_admin::router())
        .merge(meisync_admin::ui_router())
        .merge(meisync_webdav::router())
        // The S3 API is reachable both at `/s3` and at the endpoint root, which is what
        // `--endpoint-url` based clients (`aws s3`, `rclone`, `mc`) expect.
        .merge(meisync_s3::router())
        .merge(meisync_s3::router_at_root())
        .merge(meisync_ysync::router())
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(TraceLayer::new_for_http());

    if state.config.dav_at_root {
        // Convenience mount: WebDAV also answers on `/` so `davfs2` can mount the server root.
        app = app.merge(meisync_webdav::router_at_root());
    }

    app.with_state(state.as_ref().clone())
}

/// `GET /` – AWS signed requests are ListBuckets, everything else gets the service index.
async fn root(State(state): State<AppState>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    if meisync_s3::is_sigv4(&parts) {
        let request = axum::http::Request::from_parts(parts, body);
        return meisync_s3::serve(state, request).await;
    }
    index().await.into_response()
}

async fn index() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "service": "meisync",
        "description": "One File = One YDoc, exposed through WebDAV, S3 and YSync",
        "endpoints": {
            "api": "/api",
            "webdav": "/dav",
            "s3": "/s3",
            "ysync": "/ysync",
            "health": "/api/health",
            "admin": "/admin",
            "sync": "/api/sync",
        }
    }))
}

/// Boot a node: storage, namespace, document manager, auth and file service.
pub async fn bootstrap(config: ServerConfig) -> anyhow::Result<std::sync::Arc<AppState>> {
    let state = AppState::bootstrap(config).await?;
    meisync_admin::bootstrap(&state).await?;
    Ok(state)
}

/// Compose a configuration from CLI style values.
#[allow(clippy::too_many_arguments)]
pub fn config_from_parts(
    data_dir: std::path::PathBuf,
    bind: String,
    public_default_write: bool,
    max_cache_documents: usize,
    idle_unload_secs: u64,
    token_ttl_days: u64,
    max_body_mb: usize,
    compaction: CompactionPolicy,
    admin_user: Option<String>,
    admin_password: Option<String>,
    first_user_admin: bool,
) -> ServerConfig {
    ServerConfig {
        data_dir,
        bind,
        token_ttl: std::time::Duration::from_secs(token_ttl_days * 24 * 3600),
        public_default_write,
        max_body_bytes: max_body_mb * 1024 * 1024,
        documents: DocumentManagerConfig {
            max_cached_documents: max_cache_documents,
            idle_unload_after: std::time::Duration::from_secs(idle_unload_secs),
            janitor_interval: std::time::Duration::from_secs(30),
            compaction,
        },
        dav_at_root: false,
        admin_user,
        admin_password,
        first_user_admin,
    }
}
