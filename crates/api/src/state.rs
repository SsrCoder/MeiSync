use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use meisync_auth::AuthService;
use meisync_document::{DocumentManager, DocumentManagerConfig};
use meisync_persistence::fs_store::CompactionPolicy;
use meisync_persistence::Storage;

use crate::service::{DefaultFileService, FileServiceConfig};

/// Runtime configuration of a MeiSync node (`docs/mvp.md` §35).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub bind: String,
    pub token_ttl: Duration,
    pub public_default_write: bool,
    pub max_body_bytes: usize,
    pub documents: DocumentManagerConfig,
    /// Serve the WebDAV endpoint from `/` as well (handy for `davfs` / `rclone`).
    pub dav_at_root: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            bind: "127.0.0.1:8080".to_string(),
            token_ttl: Duration::from_secs(30 * 24 * 3600),
            public_default_write: false,
            max_body_bytes: 64 * 1024 * 1024,
            documents: DocumentManagerConfig::default(),
            dav_at_root: false,
        }
    }
}

impl ServerConfig {
    pub fn compaction(&self) -> CompactionPolicy {
        self.documents.compaction
    }
}

/// Everything a request handler needs.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub documents: Arc<DocumentManager>,
    pub auth: Arc<AuthService>,
    pub files: Arc<DefaultFileService>,
    pub permissions: Arc<dyn meisync_core::PermissionService>,
    pub config: Arc<ServerConfig>,
}

impl AppState {
    /// Boot a complete node: storage, namespace bootstrap, document manager, auth, file service.
    pub async fn bootstrap(config: ServerConfig) -> anyhow::Result<Arc<Self>> {
        let storage = Storage::open(&config.data_dir).await?;
        let files_repo = Arc::new(meisync_persistence::FileRepository::new(
            storage.db.clone(),
        ));
        let acl_repo = Arc::new(meisync_persistence::AclRepository::new(storage.db.clone()));
        let bucket_repo = Arc::new(meisync_persistence::BucketRepository::new(
            storage.db.clone(),
        ));
        let ydoc_repo = Arc::new(meisync_persistence::YDocRepository::new(storage.db.clone()));
        let permissions: Arc<dyn meisync_core::PermissionService> =
            Arc::new(meisync_core::VisibilityPermissionService::new(
                acl_repo.clone(),
            ));
        let documents = DocumentManager::new(
            storage.documents.clone(),
            files_repo.clone(),
            ydoc_repo.clone(),
            config.documents.clone(),
        );

        let files = DefaultFileService::new(
            files_repo,
            acl_repo,
            bucket_repo,
            documents.clone(),
            permissions.clone(),
            FileServiceConfig {
                public_default_write: config.public_default_write,
            },
        );
        files.bootstrap().await?;

        let auth = Arc::new(AuthService::new(
            storage.db.clone(),
            config.token_ttl,
        ));

        let state = Arc::new(Self {
            storage,
            documents,
            auth,
            files,
            permissions,
            config: Arc::new(config),
        });
        state.documents.spawn_janitor();
        Ok(state)
    }

    pub fn file_service(&self) -> &Arc<DefaultFileService> {
        &self.files
    }
}