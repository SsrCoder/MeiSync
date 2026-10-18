use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use meisync_core::{MeiSyncError, Result, User};

use crate::error::ApiError;
use crate::state::AppState;

/// Bearer token of the current request, if any.
///
/// WebDAV / S3 / YSync all accept the same `Authorization: Bearer <token>` header
/// (`docs/mvp.md` §11). YSync additionally accepts `?token=` because browsers cannot set headers
/// on WebSocket handshakes.
#[derive(Debug, Clone)]
pub struct AuthToken(pub String);

impl AuthToken {
    pub fn from_parts(parts: &Parts) -> Option<String> {
        if let Some(value) = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
        {
            let value = value.trim();
            if let Some(token) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
            if let Some(token) = value.strip_prefix("Token ").or_else(|| value.strip_prefix("token ")) {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
            // S3 clients may send the raw token in the Authorization header.
            if !value.contains(' ') && value.len() > 20 {
                return Some(value.to_string());
            }
        }
        None
    }

    pub fn from_query(parts: &Parts) -> Option<String> {
        let query = parts.uri.query()?;
        for pair in query.split('&') {
            let mut kv = pair.splitn(2, '=');
            let key = kv.next()?;
            if key == "token" || key == "access_token" {
                let value = kv.next()?;
                let decoded = percent_encoding::percent_decode_str(value)
                    .decode_utf8_lossy()
                    .to_string();
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
        None
    }
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| {
            let value = value.trim();
            if let Some(token) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) {
                return Some(token.trim().to_string());
            }
            None
        })
}

/// Authenticated user extracted from the bearer token.
#[derive(Debug, Clone)]
pub struct AuthUser(pub User);

impl AuthUser {
    pub fn user(&self) -> &User {
        &self.0
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = AuthToken::from_parts(parts)
            .or_else(|| AuthToken::from_query(parts))
            .ok_or_else(|| ApiError(MeiSyncError::Unauthorized("missing bearer token".into())))?;
        let user = state.auth.authenticate(&token).await?;
        Ok(AuthUser(user))
    }
}

/// Same as [`AuthUser`] but optional: anonymous requests are allowed through.
#[derive(Debug, Clone)]
pub struct MaybeAuthUser(pub Option<User>);

impl MaybeAuthUser {
    pub fn user(&self) -> Result<&User> {
        self.0
            .as_ref()
            .ok_or_else(|| MeiSyncError::Unauthorized("authentication required".into()))
    }
}

impl FromRequestParts<AppState> for MaybeAuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = AuthToken::from_parts(parts).or_else(|| AuthToken::from_query(parts));
        match token {
            Some(token) => {
                let user = state.auth.authenticate(&token).await?;
                Ok(MaybeAuthUser(Some(user)))
            }
            None => Ok(MaybeAuthUser(None)),
        }
    }
}

/// Extension trait so handlers can grab the shared state without generics noise.
pub trait AppStateExt {
    fn app(&self) -> &Arc<AppState>;
}