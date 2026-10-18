//! Authentication (`docs/mvp.md` §11): account + token.
//!
//! * `POST /api/auth/register` – create an account
//! * `POST /api/auth/login`    – obtain a bearer token
//! * every other protocol (WebDAV / S3 / YSync) consumes the token as
//!   `Authorization: Bearer <token>`
//!
//! The very same token doubles as the S3 "secret access key", so stock S3 clients keep working
//! with SigV4 signed requests.

use std::sync::Arc;
use std::time::Duration;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use meisync_core::{AuthToken, MeiSyncError, Result, Session, User, UserId};
use meisync_persistence::{SessionRepository, SqliteDatabase, UserRepository};
use rand::RngCore;

pub struct AuthService {
    users: UserRepository,
    sessions: SessionRepository,
    token_ttl: Duration,
}

impl AuthService {
    pub fn new(db: Arc<SqliteDatabase>, token_ttl: Duration) -> Self {
        Self {
            users: UserRepository::new(db.clone()),
            sessions: SessionRepository::new(db),
            token_ttl,
        }
    }

    pub fn users(&self) -> &UserRepository {
        &self.users
    }

    pub fn sessions(&self) -> &SessionRepository {
        &self.sessions
    }

    /// Create an account and immediately return a session token.
    pub async fn register(&self, username: &str, password: &str) -> Result<AuthToken> {
        if !User::is_valid_username(username) {
            return Err(MeiSyncError::bad_request(
                "username must be 1-64 chars of [a-zA-Z0-9._-]",
            ));
        }
        if password.len() < 6 {
            return Err(MeiSyncError::bad_request(
                "password must be at least 6 characters",
            ));
        }
        if self.users.find_by_username(username).await?.is_some() {
            return Err(MeiSyncError::Conflict(format!(
                "user already exists: {username}"
            )));
        }

        let user = User {
            id: UserId::generate(),
            username: username.to_string(),
            password_hash: hash_password(password)?,
            created_at: Utc::now(),
        };
        let user = self.users.create(user).await?;
        let token = self.issue_token(&user.id).await?;
        Ok(AuthToken {
            token: token.0,
            expires_at: token.1,
            user: user.to_public(),
        })
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<AuthToken> {
        let user = self
            .users
            .find_by_username(username)
            .await?
            .ok_or_else(|| MeiSyncError::Unauthorized("unknown user".into()))?;
        if !verify_password(password, &user.password_hash) {
            return Err(MeiSyncError::Unauthorized("invalid password".into()));
        }
        let (token, expires_at) = self.issue_token(&user.id).await?;
        Ok(AuthToken {
            token,
            expires_at,
            user: user.to_public(),
        })
    }

    /// Resolve a bearer token into the authenticated user.
    pub async fn authenticate(&self, token: &str) -> Result<User> {
        let session = self
            .sessions
            .find(token)
            .await?
            .ok_or_else(|| MeiSyncError::Unauthorized("invalid token".into()))?;
        if session.is_expired(Utc::now()) {
            let _ = self.sessions.delete(token).await;
            return Err(MeiSyncError::Unauthorized("token expired".into()));
        }
        self.users
            .find_by_id(&session.user_id)
            .await?
            .ok_or_else(|| MeiSyncError::Unauthorized("unknown user".into()))
    }

    pub async fn logout(&self, token: &str) -> Result<()> {
        self.sessions.delete(token).await
    }

    /// Undo a registration whose namespace provisioning failed, so a failed signup never leaves
    /// a half-created account whose name can no longer be used. The sessions go with it.
    pub async fn rollback_registration(&self, user_id: &UserId) -> Result<()> {
        self.users.delete(user_id).await
    }

    pub async fn purge_expired(&self) -> Result<usize> {
        self.sessions.delete_expired().await
    }

    async fn issue_token(&self, user_id: &UserId) -> Result<(String, chrono::DateTime<Utc>)> {
        let token = generate_token();
        let now = Utc::now();
        let expires_at = now + ChronoDuration::from_std(self.token_ttl).unwrap_or_else(|_| ChronoDuration::days(30));
        self.sessions
            .create(Session {
                token: token.clone(),
                user_id: user_id.clone(),
                created_at: now,
                expires_at,
            })
            .await?;
        Ok((token, expires_at))
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| MeiSyncError::internal(format!("password hashing failed: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 256 bit random token, URL safe.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("secret123").unwrap();
        assert!(verify_password("secret123", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn tokens_are_unique_and_long() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert!(a.len() >= 40);
    }
}