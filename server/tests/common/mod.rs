//! Shared harness for the MeiSync integration tests.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use meisync_api::AppState;
use meisync_server::{router, ServerConfig};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

pub struct Harness {
    pub state: Arc<AppState>,
    pub dir: Option<TempDir>,
    pub app: Router,
}

impl Harness {
    pub async fn spawn() -> Harness {
        Harness::in_dir(None).await
    }

    pub async fn in_dir(dir: Option<PathBuf>) -> Harness {
        Harness::with_config(dir, |_| {}).await
    }

    /// Spawn a node and let the caller tweak the configuration (limits, flags).
    pub async fn with_config(dir: Option<PathBuf>, tweak: impl FnOnce(&mut ServerConfig)) -> Harness {
        let temp = match dir {
            Some(_) => None,
            None => Some(TempDir::new().expect("temp dir")),
        };
        let data_dir = match &temp {
            Some(dir) => dir.path().to_path_buf(),
            None => dir.expect("data dir"),
        };
        let mut config = ServerConfig {
            data_dir,
            ..Default::default()
        };
        tweak(&mut config);
        let state = AppState::bootstrap(config).await.expect("bootstrap");
        let app = router(state.clone());
        Harness {
            state,
            dir: temp,
            app,
        }
    }

    pub fn request(&self, method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("request")
    }

    pub fn request_with_body(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: impl Into<Body>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        builder.body(body.into()).expect("request")
    }

    pub async fn send(&self, request: Request<Body>) -> Response<Body> {
        self.app
            .clone()
            .oneshot(request)
            .await
            .expect("router response")
    }

    pub async fn register(&self, username: &str) -> String {
        let body = serde_json::json!({"username": username, "password": "secret123"});
        let request = self
            .request_with_body(
                "POST",
                "/api/auth/register",
                None,
                Some("application/json"),
                serde_json::to_vec(&body).unwrap(),
            );
        let response = self.send(request).await;
        assert_eq!(response.status(), StatusCode::CREATED, "register {username}");
        let value: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap())
                .unwrap();
        value["token"].as_str().expect("token").to_string()
    }

    pub async fn login(&self, username: &str) -> String {
        let body = serde_json::json!({"username": username, "password": "secret123"});
        let request = self
            .request_with_body(
                "POST",
                "/api/auth/login",
                None,
                Some("application/json"),
                serde_json::to_vec(&body).unwrap(),
            );
        let response = self.send(request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap())
                .unwrap();
        value["token"].as_str().expect("token").to_string()
    }

    /// Boot a second node on the same data directory (restart simulation).
    pub async fn restart(data_dir: PathBuf) -> Harness {
        let config = ServerConfig {
            data_dir,
            ..Default::default()
        };
        let state = AppState::bootstrap(config).await.expect("bootstrap");
        let app = router(state.clone());
        Harness {
            state,
            dir: None,
            app,
        }
    }
}

pub async fn body_string(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).to_string()
}

pub async fn body_bytes(response: Response<Body>) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body")
        .to_vec()
}
/// Small helper to add headers to a request.
pub trait CloneHeader {
    fn clone_header(self, name: &str, value: &str) -> Self;
}

impl CloneHeader for axum::http::Request<Body> {
    fn clone_header(mut self, name: &str, value: &str) -> Self {
        let name = axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name");
        let value = axum::http::HeaderValue::from_str(value).expect("header value");
        self.headers_mut().insert(name, value);
        self
    }
}
