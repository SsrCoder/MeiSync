//! Read models for the administration console. All queries are intentionally read-only; writes
//! go through the normal `AuthService` / `DefaultFileService` paths so the invariants of the
//! document layer stay in one place.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use meisync_core::{Result, UserId};
use meisync_persistence::{
    AuditRecord, SessionRepository, SqliteDatabase, SyncDevice, SyncDeviceRepository,
    UserRepository,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::routes::mask_token;

fn storage<E: std::fmt::Display>(err: E) -> meisync_core::MeiSyncError {
    meisync_core::MeiSyncError::Storage(err.to_string())
}

/// Stable, opaque console identifier of a session: the first 16 hex digits of `sha256(token)`.
///
/// The console never receives the bearer token itself (it would end up in browser history, proxy
/// logs and screenshots), so `DELETE /api/admin/sessions/{id}` addresses a session by this digest
/// and the server resolves it back to the token. A truncated SHA-256 is not reversible.
pub fn session_id(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

fn from_ts(value: i64) -> DateTime<Utc> {
    use chrono::TimeZone;
    Utc.timestamp_millis_opt(value)
        .single()
        .unwrap_or_else(Utc::now)
}

#[derive(Clone)]
pub struct AdminRepository {
    db: Arc<SqliteDatabase>,
    users: UserRepository,
    sessions: SessionRepository,
    devices: SyncDeviceRepository,
    audit: meisync_persistence::AuditRepository,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStats {
    pub users: i64,
    pub admins: i64,
    pub disabled_users: i64,
    pub files: i64,
    pub collections: i64,
    pub bytes: u64,
    pub ydocs: i64,
    pub active_sessions: i64,
    pub devices: i64,
    pub audit_entries: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUserRow {
    pub id: String,
    pub username: String,
    pub role: String,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub file_count: i64,
    pub bytes: u64,
    pub session_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSessionRow {
    /// Opaque session id (`session_id`), the only handle the console gets.
    pub id: String,
    /// Masked token, so an administrator can still recognise a session (§ admin API rule).
    pub token_prefix: String,
    pub user_id: String,
    pub username: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminDeviceRow {
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub vault: String,
    pub name: String,
    pub platform: String,
    pub plugin_version: String,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl AdminRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self {
            users: UserRepository::new(db.clone()),
            sessions: SessionRepository::new(db.clone()),
            devices: SyncDeviceRepository::new(db.clone()),
            audit: meisync_persistence::AuditRepository::new(db.clone()),
            db,
        }
    }

    pub fn users(&self) -> &UserRepository {
        &self.users
    }

    pub fn sessions(&self) -> &SessionRepository {
        &self.sessions
    }

    pub fn devices(&self) -> &SyncDeviceRepository {
        &self.devices
    }

    pub fn audit(&self) -> &meisync_persistence::AuditRepository {
        &self.audit
    }

    pub async fn stats(&self) -> Result<AdminStats> {
        self.db
            .call(|conn| {
                conn.query_row(
                    "SELECT \
                        (SELECT COUNT(*) FROM users), \
                        (SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0), \
                        (SELECT COUNT(*) FROM users WHERE disabled != 0), \
                        (SELECT COUNT(*) FROM files WHERE kind = 0), \
                        (SELECT COUNT(*) FROM files WHERE kind = 1), \
                        (SELECT COALESCE(SUM(size), 0) FROM files), \
                        (SELECT COUNT(*) FROM ydoc_states), \
                        (SELECT COUNT(*) FROM sessions WHERE expires_at > ?1), \
                        (SELECT COUNT(*) FROM sync_devices), \
                        (SELECT COUNT(*) FROM audit_log)",
                    params![Utc::now().timestamp_millis()],
                    |row| {
                        Ok(AdminStats {
                            users: row.get(0)?,
                            admins: row.get(1)?,
                            disabled_users: row.get(2)?,
                            files: row.get(3)?,
                            collections: row.get(4)?,
                            bytes: row.get::<_, i64>(5)?.max(0) as u64,
                            ydocs: row.get(6)?,
                            active_sessions: row.get(7)?,
                            devices: row.get(8)?,
                            audit_entries: row.get(9)?,
                        })
                    },
                )
                .map_err(storage)
            })
            .await
    }

    pub async fn list_users(
        &self,
        search: Option<String>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<AdminUserRow>> {
        let now = Utc::now().timestamp_millis();
        let needle = search.unwrap_or_default();
        let like = format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_"));
        let limit = limit.clamp(1, 500) as i64;
        let offset = offset.min(100_000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT u.id, u.username, COALESCE(u.role, 'user'), \
                                COALESCE(u.disabled, 0), u.created_at, u.last_login_at, \
                                (SELECT COUNT(*) FROM files f WHERE f.owner_id = u.id AND f.kind = 0), \
                                (SELECT COALESCE(SUM(f.size), 0) FROM files f WHERE f.owner_id = u.id), \
                                (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?1) \
                         FROM users u \
                         WHERE (?2 = '' OR u.username LIKE ?3 ESCAPE '\\') \
                         ORDER BY u.username LIMIT ?4 OFFSET ?5",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![now, needle, like, limit, offset], |row| {
                        Ok(AdminUserRow {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            role: row.get(2)?,
                            disabled: row.get::<_, i64>(3)? != 0,
                            created_at: from_ts(row.get::<_, i64>(4)?),
                            last_login_at: row.get::<_, Option<i64>>(5)?.map(from_ts),
                            file_count: row.get(6)?,
                            bytes: row.get::<_, i64>(7)?.max(0) as u64,
                            session_count: row.get(8)?,
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    pub async fn find_user(&self, id: &str) -> Result<Option<AdminUserRow>> {
        let id = id.to_string();
        let now = Utc::now().timestamp_millis();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT u.id, u.username, COALESCE(u.role, 'user'), \
                            COALESCE(u.disabled, 0), u.created_at, u.last_login_at, \
                            (SELECT COUNT(*) FROM files f WHERE f.owner_id = u.id AND f.kind = 0), \
                            (SELECT COALESCE(SUM(f.size), 0) FROM files f WHERE f.owner_id = u.id), \
                            (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?1) \
                     FROM users u WHERE u.id = ?2",
                    params![now, id],
                    |row| {
                        Ok(AdminUserRow {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            role: row.get(2)?,
                            disabled: row.get::<_, i64>(3)? != 0,
                            created_at: from_ts(row.get::<_, i64>(4)?),
                            last_login_at: row.get::<_, Option<i64>>(5)?.map(from_ts),
                            file_count: row.get(6)?,
                            bytes: row.get::<_, i64>(7)?.max(0) as u64,
                            session_count: row.get(8)?,
                        })
                    },
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn list_sessions(&self, limit: usize) -> Result<Vec<AdminSessionRow>> {
        let now = Utc::now().timestamp_millis();
        let limit = limit.clamp(1, 1000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT s.token, s.user_id, u.username, s.created_at, s.expires_at \
                         FROM sessions s JOIN users u ON u.id = s.user_id \
                         WHERE s.expires_at > ?1 ORDER BY s.created_at DESC LIMIT ?2",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![now, limit], |row| {
                        let token: String = row.get(0)?;
                        Ok(AdminSessionRow {
                            id: session_id(&token),
                            token_prefix: mask_token(&token),
                            user_id: row.get(1)?,
                            username: row.get(2)?,
                            created_at: from_ts(row.get::<_, i64>(3)?),
                            expires_at: from_ts(row.get::<_, i64>(4)?),
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    /// Resolve a console session id back to the bearer token.
    ///
    /// The console only ever holds the digest, so the mapping happens here, over the small set of
    /// unexpired sessions. `None` means the session is unknown or already expired.
    pub async fn session_token(&self, id: &str) -> Result<Option<String>> {
        let now = Utc::now().timestamp_millis();
        let id = id.to_string();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT s.token FROM sessions s WHERE s.expires_at > ?1")
                    .map_err(storage)?;
                let tokens = stmt
                    .query_map(params![now], |row| row.get::<_, String>(0))
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(tokens.into_iter().find(|token| session_id(token) == id))
            })
            .await
    }

    pub async fn list_devices(&self, limit: usize) -> Result<Vec<AdminDeviceRow>> {
        let limit = limit.clamp(1, 1000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT d.user_id, COALESCE(u.username, ''), d.device_id, d.vault, d.name, \
                                d.platform, d.plugin_version, d.last_seen_at, d.created_at \
                         FROM sync_devices d LEFT JOIN users u ON u.id = d.user_id \
                         ORDER BY d.last_seen_at DESC LIMIT ?1",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![limit], |row| {
                        Ok(AdminDeviceRow {
                            user_id: row.get(0)?,
                            username: row.get(1)?,
                            device_id: row.get(2)?,
                            vault: row.get(3)?,
                            name: row.get(4)?,
                            platform: row.get(5)?,
                            plugin_version: row.get(6)?,
                            last_seen_at: from_ts(row.get::<_, i64>(7)?),
                            created_at: from_ts(row.get::<_, i64>(8)?),
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    pub async fn delete_device(&self, user_id: &UserId, device_id: &str) -> Result<()> {
        let user_id = user_id.0.clone();
        let device_id = device_id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM sync_devices WHERE user_id = ?1 AND device_id = ?2",
                    params![user_id, device_id],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn list_audit(&self, limit: usize) -> Result<Vec<AuditRecord>> {
        self.audit.list(limit).await
    }

    pub async fn record_audit(
        &self,
        actor_id: Option<String>,
        actor_name: Option<String>,
        action: &str,
        target: Option<String>,
        detail: Option<String>,
    ) -> Result<()> {
        self.audit
            .record(actor_id, actor_name, action, target, detail)
            .await
    }

    pub async fn list_sync_devices_for_user(&self, user_id: &UserId) -> Result<Vec<SyncDevice>> {
        self.devices.list_for_user(user_id).await
    }
}

/// Recursively sum the size of a directory. Used for the "disk usage" tile; best effort.
pub fn directory_size(root: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
