//! Embedded administration console. The assets are part of the binary, so a node started from a
//! release build has the UI without a separate frontend deployment.

use axum::http::header::{self, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use meisync_api::AppState;

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLES_CSS: &str = include_str!("../web/styles.css");

pub fn ui_router() -> Router<AppState> {
    Router::new()
        .route("/admin", get(index))
        .route("/admin/", get(index))
        .route("/admin/app.js", get(app_js))
        .route("/admin/styles.css", get(styles))
        .route("/admin/api-info", get(crate::routes::api_info))
}

async fn index() -> Response {
    asset(INDEX, "text/html; charset=utf-8")
}

async fn app_js() -> Response {
    asset(APP_JS, "text/javascript; charset=utf-8")
}

async fn styles() -> Response {
    asset(STYLES_CSS, "text/css; charset=utf-8")
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // The console contains no inline scripts. Inline styles are used by the rendering helpers,
    // so style-src keeps 'unsafe-inline' while scripts stay restricted to this origin.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
