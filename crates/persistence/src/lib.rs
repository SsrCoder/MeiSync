//! Persistence layer: SQLite (metadata / auth / ACL) + local filesystem (YDoc snapshots and
//! update logs), see `docs/mvp.md` §14 – §20 and §32.
//!
//! SQLite never carries large BLOBs: YDoc payloads live under
//! `data/documents/<file-id>/{snapshot,updates/}`.

pub mod db;
pub mod fs_store;
pub mod repos;

pub use db::{SqliteDatabase, DATABASE_FILE_NAME};
pub use fs_store::{
    DocumentStore, LocalDocumentStore, StoreStats, UpdateSeq, DOCS_DIR_NAME, SNAPSHOT_FILE,
    UPDATES_DIR_NAME,
};
pub use repos::{
    AclRepository, AuditRecord, AuditRepository, BucketRepository, FileRepository,
    SessionRepository, SyncDevice, SyncDeviceRepository, UserFlags, UserRepository, YDocRecord,
    YDocRepository,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use meisync_core::AclProvider;

use meisync_core::{MeiSyncError, PermissionService, Result, VisibilityPermissionService};

/// Everything that touches the disk, wired together once at startup.
pub struct Storage {
    pub db: Arc<SqliteDatabase>,
    pub documents: Arc<dyn DocumentStore>,
}

impl Storage {
    /// Open (and create) the whole data directory layout at `<root>`.
    pub async fn open(root: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(root)?;
        let db = Arc::new(SqliteDatabase::open(&root.join(DATABASE_FILE_NAME))?);
        let documents: Arc<dyn DocumentStore> =
            Arc::new(LocalDocumentStore::new(root.join(DOCS_DIR_NAME)));
        Ok(Arc::new(Self { db, documents }))
    }

    /// The permission service used by every protocol (`docs/mvp.md` §13).
    pub fn permission_service(&self) -> impl PermissionService {
        VisibilityPermissionService::new(AclRepository::new(self.db.clone()))
    }
}

impl Storage {
    /// Shared permission service boxed as a trait object, handy for axum state.
    pub fn permissions(&self) -> Arc<dyn PermissionService> {
        Arc::new(VisibilityPermissionService::new(AclRepository::new(
            self.db.clone(),
        )))
    }
}

/// Resolve the data directory, honouring `MEISYNC_DATA_DIR`.
pub fn data_dir_from_env() -> PathBuf {
    std::env::var("MEISYNC_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data"))
}

/// Map a storage failure to the shared error type.
pub fn storage_err<E: std::fmt::Display>(err: E) -> MeiSyncError {
    MeiSyncError::Storage(err.to_string())
}
