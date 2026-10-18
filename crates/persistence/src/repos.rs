use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use meisync_core::{
    AclEntry, AclProvider, FileId, FileKind, FileMetadata, MeiSyncError, PermissionSet, Result,
    Session, SyncMode, User, UserId, Visibility,
};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::db::SqliteDatabase;
use crate::fs_store::UpdateSeq;

fn storage<E: std::fmt::Display>(err: E) -> MeiSyncError {
    MeiSyncError::Storage(err.to_string())
}

fn ts(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

fn from_ts(value: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(value)
        .single()
        .unwrap_or_else(Utc::now)
}

// ---------------------------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct UserRepository {
    db: Arc<SqliteDatabase>,
}

impl UserRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn create(&self, user: User) -> Result<User> {
        let u = user.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO users (id, username, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![u.id.0, u.username, u.password_hash, ts(u.created_at)],
                )
                .map_err(storage)?;
                Ok(u)
            })
            .await
    }

    pub async fn find_by_username(&self, username: &str) -> Result<Option<User>> {
        let name = username.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT id, username, password_hash, created_at FROM users WHERE username = ?1",
                    params![name],
                    row_to_user,
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn find_by_id(&self, id: &UserId) -> Result<Option<User>> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT id, username, password_hash, created_at FROM users WHERE id = ?1",
                    params![id],
                    row_to_user,
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn list(&self) -> Result<Vec<User>> {
        self.db
            .call(|conn| {
                let mut stmt = conn
                    .prepare("SELECT id, username, password_hash, created_at FROM users ORDER BY username")
                    .map_err(storage)?;
                let rows = stmt
                    .query_map([], row_to_user)
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    /// Remove an account and, through `ON DELETE CASCADE`, its sessions.
    pub async fn delete(&self, id: &UserId) -> Result<()> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM users WHERE id = ?1", params![id])
                    .map_err(storage)?;
                Ok(())
            })
            .await
    }

    /// Administrative view of the account flags that are not part of the domain `User`.
    pub async fn flags(&self, id: &UserId) -> Result<UserFlags> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT role, disabled, last_login_at FROM users WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok(UserFlags {
                            role: row.get(0)?,
                            disabled: row.get::<_, i64>(1)? != 0,
                            last_login_at: row.get::<_, Option<i64>>(2)?.map(from_ts),
                        })
                    },
                )
                .optional()
                .map_err(storage)
                .map(|row| row.unwrap_or_default())
            })
            .await
    }

    pub async fn set_role(&self, id: &UserId, role: &str) -> Result<()> {
        let id = id.0.clone();
        let role = role.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE users SET role = ?2 WHERE id = ?1",
                    params![id, role],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn set_disabled(&self, id: &UserId, disabled: bool) -> Result<()> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE users SET disabled = ?2 WHERE id = ?1",
                    params![id, if disabled { 1i64 } else { 0i64 }],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn set_password_hash(&self, id: &UserId, hash: &str) -> Result<()> {
        let id = id.0.clone();
        let hash = hash.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                    params![id, hash],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn touch_login(&self, id: &UserId) -> Result<()> {
        let id = id.0.clone();
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE users SET last_login_at = ?2 WHERE id = ?1",
                    params![id, now],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn is_admin(&self, id: &UserId) -> Result<bool> {
        Ok(self.flags(id).await?.role == "admin")
    }

    pub async fn count_admins(&self) -> Result<i64> {
        self.db
            .call(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage)
            })
            .await
    }

    pub async fn count(&self) -> Result<i64> {
        self.db
            .call(|conn| {
                conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
                    .map_err(storage)
            })
            .await
    }
}

/// Account fields used by authentication and the admin console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFlags {
    pub role: String,
    pub disabled: bool,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl Default for UserFlags {
    fn default() -> Self {
        Self {
            role: "user".to_string(),
            disabled: false,
            last_login_at: None,
        }
    }
}

fn row_to_user(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: UserId::new(row.get::<_, String>(0)?),
        username: row.get(1)?,
        password_hash: row.get(2)?,
        created_at: from_ts(row.get::<_, i64>(3)?),
    })
}

// ---------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct SessionRepository {
    db: Arc<SqliteDatabase>,
}

impl SessionRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn create(&self, session: Session) -> Result<()> {
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
                    params![session.token, session.user_id.0, ts(session.created_at), ts(session.expires_at)],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn find(&self, token: &str) -> Result<Option<Session>> {
        let token = token.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT token, user_id, created_at, expires_at FROM sessions WHERE token = ?1",
                    params![token],
                    |row| {
                        Ok(Session {
                            token: row.get(0)?,
                            user_id: UserId::new(row.get::<_, String>(1)?),
                            created_at: from_ts(row.get::<_, i64>(2)?),
                            expires_at: from_ts(row.get::<_, i64>(3)?),
                        })
                    },
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn delete(&self, token: &str) -> Result<()> {
        let token = token.to_string();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM sessions WHERE token = ?1", params![token])
                    .map_err(storage)?;
                Ok(())
            })
            .await
    }

    /// All (non expired) sessions of a user – used to verify AWS SigV4 signatures where the
    /// session token is the S3 secret key.
    pub async fn list_for_user(&self, user_id: &UserId) -> Result<Vec<Session>> {
        let user_id = user_id.0.clone();
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT token, user_id, created_at, expires_at FROM sessions \
                         WHERE user_id = ?1 AND expires_at > ?2 ORDER BY created_at",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![user_id, now], |row| {
                        Ok(Session {
                            token: row.get(0)?,
                            user_id: UserId::new(row.get::<_, String>(1)?),
                            created_at: from_ts(row.get::<_, i64>(2)?),
                            expires_at: from_ts(row.get::<_, i64>(3)?),
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    pub async fn delete_expired(&self) -> Result<usize> {
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])
                    .map_err(storage)
            })
            .await
    }

    pub async fn delete_for_user(&self, user_id: &UserId) -> Result<usize> {
        let user_id = user_id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])
                    .map_err(storage)
            })
            .await
    }

    /// Every non expired session, newest first (admin console).
    pub async fn list_active(&self, limit: usize) -> Result<Vec<Session>> {
        let now = ts(Utc::now());
        let limit = limit.clamp(1, 1000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT token, user_id, created_at, expires_at FROM sessions \
                         WHERE expires_at > ?1 ORDER BY created_at DESC LIMIT ?2",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![now, limit], |row| {
                        Ok(Session {
                            token: row.get(0)?,
                            user_id: UserId::new(row.get::<_, String>(1)?),
                            created_at: from_ts(row.get::<_, i64>(2)?),
                            expires_at: from_ts(row.get::<_, i64>(3)?),
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }
}

// ---------------------------------------------------------------------------------------------
// Files / namespace
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct FileRepository {
    db: Arc<SqliteDatabase>,
}

const FILE_COLUMNS: &str = "id, parent_id, name, owner_id, visibility, content_type, kind, \
                            created_at, updated_at, size, etag, version, sync_mode";

impl FileRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn insert(&self, meta: FileMetadata) -> Result<FileMetadata> {
        let m = meta.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO files (id, parent_id, name, owner_id, visibility, content_type, kind, \
                                        created_at, updated_at, size, etag, version, sync_mode) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        m.id.0,
                        m.parent_id.as_ref().map(|p| p.0.clone()),
                        m.name,
                        m.owner_id.0,
                        m.visibility.as_i64(),
                        m.content_type,
                        m.kind.as_i64(),
                        ts(m.created_at),
                        ts(m.updated_at),
                        m.size as i64,
                        m.etag,
                        m.version,
                        m.sync_mode.as_i64(),
                    ],
                )
                .map_err(storage)?;
                conn.execute(
                    "INSERT INTO ydoc_states (file_id, snapshot_version, update_seq, updated_at, sync_mode) \
                     VALUES (?1, 0, 0, ?2, ?3)",
                    params![m.id.0, ts(m.updated_at), m.sync_mode.as_i64()],
                )
                .map_err(storage)?;
                Ok(m)
            })
            .await
    }

    pub async fn find_by_id(&self, id: &FileId) -> Result<Option<FileMetadata>> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1"),
                    params![id],
                    row_to_file,
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn find_child(&self, parent: &FileId, name: &str) -> Result<Option<FileMetadata>> {
        let parent = parent.0.clone();
        let name = name.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT {FILE_COLUMNS} FROM files WHERE parent_id = ?1 AND name = ?2"),
                    params![parent, name],
                    row_to_file,
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn list_children(&self, parent: &FileId) -> Result<Vec<FileMetadata>> {
        let parent = parent.0.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {FILE_COLUMNS} FROM files WHERE parent_id = ?1 ORDER BY kind, name"
                    ))
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![parent], row_to_file)
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    /// Depth first traversal used by recursive delete / move checks.
    pub async fn list_descendants(&self, id: &FileId) -> Result<Vec<FileMetadata>> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(&format!(
                        "WITH RECURSIVE sub(id) AS ( \
                            SELECT id FROM files WHERE parent_id = ?1 \
                            UNION ALL \
                            SELECT f.id FROM files f JOIN sub s ON f.parent_id = s.id \
                         ) \
                         SELECT {FILE_COLUMNS} FROM files WHERE id IN (SELECT id FROM sub)"
                    ))
                    .map_err(storage)?;
                let mut rows = stmt
                    .query_map(params![id], row_to_file)
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                // Guarantee children-after-parent ordering.
                rows.sort_by_key(|m| m.id.0.clone());
                Ok(rows)
            })
            .await
    }

    /// Materialised content bookkeeping + version bump after every YDoc transaction.
    pub async fn touch_content(
        &self,
        id: &FileId,
        size: u64,
        etag: String,
        content_type: Option<String>,
        sync_mode: Option<SyncMode>,
    ) -> Result<i64> {
        let id = id.0.clone();
        let size = size as i64;
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE files SET size = ?2, etag = ?3, updated_at = ?4, version = version + 1, \
                        content_type = COALESCE(?5, content_type), \
                        sync_mode = COALESCE(?6, sync_mode) \
                     WHERE id = ?1",
                    params![id, size, etag, now, content_type, sync_mode.map(|m| m.as_i64())],
                )
                .map_err(storage)?;
                let version: i64 = conn
                    .query_row("SELECT version FROM files WHERE id = ?1", params![id], |r| r.get(0))
                    .optional()
                    .map_err(storage)?
                    .unwrap_or(0);
                Ok(version)
            })
            .await
    }

    /// Metadata only update (rename / move / visibility / content type).
    pub async fn update_meta(&self, meta: &FileMetadata) -> Result<()> {
        let m = meta.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE files SET parent_id = ?2, name = ?3, content_type = ?4, visibility = ?5, \
                        updated_at = ?6, sync_mode = ?7 WHERE id = ?1",
                    params![
                        m.id.0,
                        m.parent_id.as_ref().map(|p| p.0.clone()),
                        m.name,
                        m.content_type,
                        m.visibility.as_i64(),
                        ts(m.updated_at),
                        m.sync_mode.as_i64(),
                    ],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn set_visibility(&self, id: &FileId, visibility: Visibility) -> Result<()> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE files SET visibility = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, visibility.as_i64(), ts(Utc::now())],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    /// Deletes the row; children are removed through `ON DELETE CASCADE`.
    pub async fn delete(&self, id: &FileId) -> Result<()> {
        let id = id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM ydoc_states WHERE file_id = ?1", params![id])
                    .map_err(storage)?;
                conn.execute("DELETE FROM files WHERE id = ?1", params![id])
                    .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn count(&self) -> Result<i64> {
        self.db
            .call(|conn| {
                conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                    .map_err(storage)
            })
            .await
    }
}

fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileMetadata> {
    let visibility: i64 = row.get(4)?;
    let kind: i64 = row.get(6)?;
    let sync_mode: i64 = row.get(12)?;
    Ok(FileMetadata {
        id: FileId::new(row.get::<_, String>(0)?),
        parent_id: row.get::<_, Option<String>>(1)?.map(FileId::new),
        name: row.get(2)?,
        owner_id: UserId::new(row.get::<_, String>(3)?),
        visibility: Visibility::from_i64(visibility),
        content_type: row.get(5)?,
        kind: FileKind::from_i64(kind),
        created_at: from_ts(row.get::<_, i64>(7)?),
        updated_at: from_ts(row.get::<_, i64>(8)?),
        size: row.get::<_, i64>(9)?.max(0) as u64,
        etag: row.get(10)?,
        version: row.get(11)?,
        sync_mode: SyncMode::from_i64(sync_mode),
    })
}

// ---------------------------------------------------------------------------------------------
// ACL – the File Permission layer (`docs/mvp.md` §13)
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct AclRepository {
    db: Arc<SqliteDatabase>,
}

impl AclRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AclProvider for AclRepository {
    async fn acl_permissions(&self, file_id: &FileId, user_id: &UserId) -> Result<PermissionSet> {
        let file_id = file_id.0.clone();
        let user_id = user_id.0.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT permissions FROM acl WHERE file_id = ?1 AND user_id = ?2")
                    .map_err(storage)?;
                let mut set = PermissionSet::NONE;
                let rows = stmt
                    .query_map(params![file_id, user_id], |r| r.get::<_, i64>(0))
                    .map_err(storage)?;
                for row in rows {
                    set.insert(PermissionSet(row.map_err(storage)? as u8));
                }
                Ok(set)
            })
            .await
    }

    async fn acl_list(&self, file_id: &FileId) -> Result<Vec<AclEntry>> {
        let file_id = file_id.0.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT file_id, user_id, permissions FROM acl WHERE file_id = ?1")
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![file_id], |row| {
                        Ok(AclEntry {
                            file_id: FileId::new(row.get::<_, String>(0)?),
                            user_id: UserId::new(row.get::<_, String>(1)?),
                            permissions: PermissionSet(row.get::<_, i64>(2)? as u8),
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    async fn acl_set(
        &self,
        file_id: &FileId,
        user_id: &UserId,
        permissions: PermissionSet,
    ) -> Result<()> {
        let file_id = file_id.0.clone();
        let user_id = user_id.0.clone();
        let value = permissions.0 as i64;
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                if permissions.is_empty() {
                    conn.execute(
                        "DELETE FROM acl WHERE file_id = ?1 AND user_id = ?2",
                        params![file_id, user_id],
                    )
                    .map_err(storage)?;
                } else {
                    conn.execute(
                        "INSERT INTO acl (file_id, user_id, permissions, created_at) VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(file_id, user_id) DO UPDATE SET permissions = ?3",
                        params![file_id, user_id, value, now],
                    )
                    .map_err(storage)?;
                }
                Ok(())
            })
            .await
    }
}

// ---------------------------------------------------------------------------------------------
// YDoc persistence metadata (`docs/mvp.md` §15)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YDocRecord {
    pub file_id: FileId,
    /// Number of updates folded into the stored snapshot.
    pub snapshot_version: u64,
    /// Highest server side persistence sequence written to disk (NOT a Yjs clock).
    pub update_seq: u64,
    pub updated_at: DateTime<Utc>,
    pub sync_mode: SyncMode,
}

#[derive(Clone)]
pub struct YDocRepository {
    db: Arc<SqliteDatabase>,
}

impl YDocRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn get(&self, file_id: &FileId) -> Result<Option<YDocRecord>> {
        let file_id = file_id.0.clone();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT file_id, snapshot_version, update_seq, updated_at, sync_mode \
                         FROM ydoc_states WHERE file_id = ?1",
                    params![file_id],
                    |row| {
                        Ok(YDocRecord {
                            file_id: FileId::new(row.get::<_, String>(0)?),
                            snapshot_version: row.get::<_, i64>(1)?.max(0) as u64,
                            update_seq: row.get::<_, i64>(2)?.max(0) as u64,
                            updated_at: from_ts(row.get::<_, i64>(3)?),
                            sync_mode: SyncMode::from_i64(row.get::<_, i64>(4)?),
                        })
                    },
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn record_update(&self, file_id: &FileId, seq: UpdateSeq) -> Result<()> {
        let file_id = file_id.0.clone();
        let seq = seq as i64;
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE ydoc_states SET update_seq = MAX(update_seq, ?2), updated_at = ?3 \
                     WHERE file_id = ?1",
                    params![file_id, seq, ts(Utc::now())],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn record_snapshot(&self, file_id: &FileId, through_seq: UpdateSeq) -> Result<()> {
        // Only the snapshot bookkeeping changes here: the sync mode is a property of the
        // document (``set_sync_mode``) and must survive every checkpoint / compaction.
        let file_id = file_id.0.clone();
        let seq = through_seq as i64;
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE ydoc_states SET snapshot_version = snapshot_version + 1, \
                        update_seq = ?2, updated_at = ?3 WHERE file_id = ?1",
                    params![file_id, seq, ts(Utc::now())],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn set_sync_mode(&self, file_id: &FileId, mode: SyncMode) -> Result<()> {
        let file_id = file_id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE ydoc_states SET sync_mode = ?2 WHERE file_id = ?1",
                    params![file_id, mode.as_i64()],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn delete(&self, file_id: &FileId) -> Result<()> {
        let file_id = file_id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM ydoc_states WHERE file_id = ?1",
                    params![file_id],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }
}

// ---------------------------------------------------------------------------------------------
// Buckets (S3 address space, `docs/mvp.md` §30)
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct BucketRepository {
    db: Arc<SqliteDatabase>,
}

impl BucketRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn upsert(
        &self,
        name: &str,
        collection_id: &FileId,
        owner_id: Option<&UserId>,
    ) -> Result<()> {
        let name = name.to_string();
        let collection_id = collection_id.0.clone();
        let owner_id = owner_id.map(|o| o.0.clone());
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO buckets (name, collection_id, owner_id, created_at) \
                     VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(name) DO UPDATE SET collection_id = ?2, owner_id = ?3",
                    params![name, collection_id, owner_id, ts(Utc::now())],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn find(&self, name: &str) -> Result<Option<FileId>> {
        let name = name.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT collection_id FROM buckets WHERE name = ?1",
                    params![name],
                    |row| Ok(FileId::new(row.get::<_, String>(0)?)),
                )
                .optional()
                .map_err(storage)
            })
            .await
    }

    pub async fn delete_for_collection(&self, collection_id: &FileId) -> Result<()> {
        let collection_id = collection_id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM buckets WHERE collection_id = ?1",
                    params![collection_id],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn list(&self) -> Result<Vec<(String, FileId, Option<UserId>)>> {
        self.db
            .call(|conn| {
                let mut stmt = conn
                    .prepare("SELECT name, collection_id, owner_id FROM buckets ORDER BY name")
                    .map_err(storage)?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            FileId::new(row.get::<_, String>(1)?),
                            row.get::<_, Option<String>>(2)?.map(UserId::new),
                        ))
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }
}
// ---------------------------------------------------------------------------------------------
// Audit log (admin console + security relevant actions)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_id: Option<String>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone)]
pub struct AuditRepository {
    db: Arc<SqliteDatabase>,
}

impl AuditRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn record(
        &self,
        actor_id: Option<String>,
        actor_name: Option<String>,
        action: &str,
        target: Option<String>,
        detail: Option<String>,
    ) -> Result<()> {
        let action = action.to_string();
        let now = ts(Utc::now());
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO audit_log (at, actor_id, actor_name, action, target, detail) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![now, actor_id, actor_name, action, target, detail],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn list(&self, limit: usize) -> Result<Vec<AuditRecord>> {
        let limit = limit.clamp(1, 1000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, at, actor_id, actor_name, action, target, detail \
                         FROM audit_log ORDER BY id DESC LIMIT ?1",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![limit], |row| {
                        Ok(AuditRecord {
                            id: row.get(0)?,
                            at: from_ts(row.get::<_, i64>(1)?),
                            actor_id: row.get(2)?,
                            actor_name: row.get(3)?,
                            action: row.get(4)?,
                            target: row.get(5)?,
                            detail: row.get(6)?,
                        })
                    })
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }
}

// ---------------------------------------------------------------------------------------------
// Sync devices (Obsidian plugin / desktop clients)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDevice {
    pub user_id: UserId,
    pub device_id: String,
    pub vault: String,
    pub name: String,
    pub platform: String,
    pub plugin_version: String,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct SyncDeviceRepository {
    db: Arc<SqliteDatabase>,
}

impl SyncDeviceRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    pub async fn upsert(&self, device: &SyncDevice) -> Result<()> {
        let device = device.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO sync_devices \
                        (user_id, device_id, vault, name, platform, plugin_version, last_seen_at, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT(user_id, device_id, vault) DO UPDATE SET \
                        name = ?4, platform = ?5, plugin_version = ?6, last_seen_at = ?7",
                    params![
                        device.user_id.0,
                        device.device_id,
                        device.vault,
                        device.name,
                        device.platform,
                        device.plugin_version,
                        ts(device.last_seen_at),
                        ts(device.created_at),
                    ],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }

    pub async fn list_for_user(&self, user_id: &UserId) -> Result<Vec<SyncDevice>> {
        let user_id = user_id.0.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT user_id, device_id, vault, name, platform, plugin_version, \
                                last_seen_at, created_at \
                         FROM sync_devices WHERE user_id = ?1 ORDER BY last_seen_at DESC",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![user_id], row_to_sync_device)
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    pub async fn list_all(&self, limit: usize) -> Result<Vec<SyncDevice>> {
        let limit = limit.clamp(1, 1000) as i64;
        self.db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT user_id, device_id, vault, name, platform, plugin_version, \
                                last_seen_at, created_at \
                         FROM sync_devices ORDER BY last_seen_at DESC LIMIT ?1",
                    )
                    .map_err(storage)?;
                let rows = stmt
                    .query_map(params![limit], row_to_sync_device)
                    .map_err(storage)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage)?;
                Ok(rows)
            })
            .await
    }

    pub async fn delete_for_user(&self, user_id: &UserId) -> Result<()> {
        let user_id = user_id.0.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM sync_devices WHERE user_id = ?1",
                    params![user_id],
                )
                .map_err(storage)?;
                Ok(())
            })
            .await
    }
}

fn row_to_sync_device(row: &Row<'_>) -> rusqlite::Result<SyncDevice> {
    Ok(SyncDevice {
        user_id: UserId::new(row.get::<_, String>(0)?),
        device_id: row.get(1)?,
        vault: row.get(2)?,
        name: row.get(3)?,
        platform: row.get(4)?,
        plugin_version: row.get(5)?,
        last_seen_at: from_ts(row.get::<_, i64>(6)?),
        created_at: from_ts(row.get::<_, i64>(7)?),
    })
}
