use std::path::Path;
use std::sync::Arc;

use meisync_core::{MeiSyncError, Result};
use rusqlite::{Connection, OpenFlags};
use tokio::task::spawn_blocking;

pub const DATABASE_FILE_NAME: &str = "meisync.db";

/// Thin async wrapper around a single SQLite connection.
///
/// A single writer connection is enough for the single node MVP deployment described in
/// `docs/mvp.md` §35 and keeps `BEGIN IMMEDIATE` transactions trivially correct. Every statement
/// runs inside `spawn_blocking`, so the async runtime is never blocked by disk IO.
#[derive(Clone)]
pub struct SqliteDatabase {
    conn: Arc<std::sync::Mutex<Connection>>,
}

impl SqliteDatabase {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .map_err(storage)?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let db = Self {
            conn: Arc::new(std::sync::Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// In-memory database, used by tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(storage)?;
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let db = Self {
            conn: Arc::new(std::sync::Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Run `f` with the connection, off the async runtime.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            f(&guard)
        })
        .await
        .map_err(storage)?
    }

    /// Run `f` inside an `IMMEDIATE` transaction.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        spawn_blocking(move || {
            let mut guard = conn.lock().unwrap();
            let tx = guard
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let out = f(&tx)?;
            tx.commit().map_err(storage)?;
            Ok(out)
        })
        .await
        .map_err(storage)?
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA).map_err(storage)?;
        // Columns added after the MVP schema. `CREATE TABLE IF NOT EXISTS` never changes an
        // existing table, so a node that was booted before this feature keeps its users table
        // and only gets the new columns.
        ensure_column(&conn, "users", "role", "TEXT NOT NULL DEFAULT 'user'")?;
        ensure_column(&conn, "users", "disabled", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "users", "last_login_at", "INTEGER")?;
        Ok(())
    }
}

fn storage<E: std::fmt::Display>(err: E) -> MeiSyncError {
    MeiSyncError::Storage(err.to_string())
}

/// Add a column to an existing table when it is missing. SQLite has no `ADD COLUMN IF NOT
/// EXISTS`, so the current shape is inspected first.
fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(storage)?;
    for name in rows {
        if name.map_err(storage)? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )
    .map_err(storage)?;
    Ok(())
}

/// The MVP schema (`docs/mvp.md` §15).
///
/// Beyond the tables listed in the design document we keep the columns that make directory
/// listings cheap (`kind`, `size`, `etag`, `version`, `sync_mode`) plus the `acl` and `buckets`
/// tables required by the Permission Service and the S3 API.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    role          TEXT NOT NULL DEFAULT 'user',
    disabled      INTEGER NOT NULL DEFAULT 0,
    last_login_at INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
    token      TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);

CREATE TABLE IF NOT EXISTS files (
    id           TEXT PRIMARY KEY,
    parent_id    TEXT REFERENCES files(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    owner_id     TEXT NOT NULL,
    visibility   INTEGER NOT NULL,
    content_type TEXT,
    kind         INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    size         INTEGER NOT NULL DEFAULT 0,
    etag         TEXT,
    version      INTEGER NOT NULL DEFAULT 0,
    sync_mode    INTEGER NOT NULL DEFAULT 0,
    UNIQUE(parent_id, name)
);
CREATE INDEX IF NOT EXISTS idx_files_parent ON files(parent_id);

CREATE TABLE IF NOT EXISTS acl (
    file_id     TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    permissions INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (file_id, user_id)
);

CREATE TABLE IF NOT EXISTS ydoc_states (
    file_id          TEXT PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    snapshot_version INTEGER NOT NULL,
    update_seq       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    sync_mode        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS buckets (
    name          TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL,
    owner_id      TEXT,
    created_at    INTEGER NOT NULL
);

-- Administrative audit trail. Best effort: a write failure never fails the action itself.
CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    actor_id   TEXT,
    actor_name TEXT,
    action     TEXT NOT NULL,
    target     TEXT,
    detail     TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_log_at ON audit_log(at DESC);

-- Obsidian / sync clients. One row per (account, device, remote root).
CREATE TABLE IF NOT EXISTS sync_devices (
    user_id        TEXT NOT NULL,
    device_id      TEXT NOT NULL,
    vault          TEXT NOT NULL,
    name           TEXT NOT NULL,
    platform       TEXT NOT NULL DEFAULT '',
    plugin_version TEXT NOT NULL DEFAULT '',
    last_seen_at   INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (user_id, device_id, vault)
);
CREATE INDEX IF NOT EXISTS idx_sync_devices_user ON sync_devices(user_id);
CREATE INDEX IF NOT EXISTS idx_sync_devices_seen ON sync_devices(last_seen_at DESC);
"#;

/// Helper to build an owned `Arc<SqliteDatabase>` from a `&SqliteDatabase`.
pub fn shared(db: &SqliteDatabase) -> Arc<SqliteDatabase> {
    Arc::new(db.clone())
}
