use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use meisync_core::{FileId, FileMetadata, Result, SyncMode};
use meisync_persistence::fs_store::{CompactionPolicy, DocumentStore};
use meisync_persistence::{FileRepository, YDocRepository};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::loaded::{restore_document, DocumentHandle};
use crate::room::{ConnId, Room, RoomEvent};

/// Tuning knobs for the YDoc cache / lifecycle (`docs/mvp.md` §22, §23, §35 §36).
#[derive(Debug, Clone)]
pub struct DocumentManagerConfig {
    /// Maximum number of YDocs kept in memory.
    pub max_cached_documents: usize,
    /// Documents without subscribers are unloaded after this idle period.
    pub idle_unload_after: Duration,
    /// Interval of the background cache maintenance task.
    pub janitor_interval: Duration,
    pub compaction: CompactionPolicy,
}

impl Default for DocumentManagerConfig {
    fn default() -> Self {
        Self {
            max_cached_documents: 512,
            idle_unload_after: Duration::from_secs(300),
            janitor_interval: Duration::from_secs(30),
            compaction: CompactionPolicy::default(),
        }
    }
}

#[derive(Default)]
struct Cache {
    entries: HashMap<FileId, DocumentHandle>,
    /// Least-recently-used first.
    order: Vec<FileId>,
}
impl Cache {
    fn get(&mut self, file_id: &FileId) -> Option<DocumentHandle> {
        let handle = self.entries.get(file_id)?.clone();
        self.touch(file_id);
        Some(handle)
    }

    fn touch(&mut self, file_id: &FileId) {
        if let Some(pos) = self.order.iter().position(|id| id == file_id) {
            let id = self.order.remove(pos);
            self.order.push(id);
        }
    }

    fn insert(&mut self, handle: DocumentHandle) {
        let file_id = handle.file_id.clone();
        self.order.retain(|id| id != &file_id);
        self.order.push(file_id.clone());
        self.entries.insert(file_id, handle);
    }

    fn remove(&mut self, file_id: &FileId) -> Option<DocumentHandle> {
        self.order.retain(|id| id != file_id);
        self.entries.remove(file_id)
    }

    fn lru_order(&self) -> Vec<FileId> {
        self.order.clone()
    }
}

/// The heart of MeiSync (`docs/mvp.md` §21).
///
/// * `FileId -> YDoc` resolution and loading
/// * single-instance-per-file concurrency control (§24)
/// * YDoc cache with LRU + idle unload (§23)
/// * rooms and update broadcast (§9)
/// * persistence: snapshot, update log, compaction (§17 – §20)
#[derive(Clone)]
pub struct DocumentManager {
    store: Arc<dyn DocumentStore>,
    files: Arc<FileRepository>,
    ydocs: Arc<YDocRepository>,
    cache: Arc<Mutex<Cache>>,
    /// One load mutex per `FileId` that is currently being loaded (single flight, §24).
    loading: Arc<Mutex<HashMap<FileId, Arc<Mutex<()>>>>>,
    rooms: Arc<Mutex<HashMap<FileId, Arc<Room>>>>,
    config: Arc<DocumentManagerConfig>,
}

impl DocumentManager {
    pub fn new(
        store: Arc<dyn DocumentStore>,
        files: Arc<FileRepository>,
        ydocs: Arc<YDocRepository>,
        config: DocumentManagerConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            files,
            ydocs,
            cache: Arc::new(Mutex::new(Cache::default())),
            loading: Arc::new(Mutex::new(HashMap::new())),
            rooms: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
        })
    }

    pub fn config(&self) -> &DocumentManagerConfig {
        &self.config
    }

    pub fn store(&self) -> &Arc<dyn DocumentStore> {
        &self.store
    }

    /// The room of a document, created on demand (§9).
    pub async fn room(&self, file_id: &FileId) -> Arc<Room> {
        let mut rooms = self.rooms.lock().await;
        rooms
            .entry(file_id.clone())
            .or_insert_with(|| Room::new(file_id.clone()))
            .clone()
    }

    /// Get the loaded YDoc of a file, loading it from disk on first access (§22).
    ///
    /// Concurrent callers of a cold file are serialised per `FileId`, so exactly one `yrs::Doc`
    /// is built and every caller receives the very same handle (§24).
    pub async fn open(&self, meta: &FileMetadata) -> Result<DocumentHandle> {
        if let Some(handle) = self.cache.lock().await.get(&meta.id) {
            return Ok(handle);
        }

        // Single flight: the loader of a cold file holds the per-file mutex, everybody else
        // waits for it and then finds the handle in the cache.
        let load_lock = {
            let mut loading = self.loading.lock().await;
            loading
                .entry(meta.id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = load_lock.lock().await;
        let cached = self.cache.lock().await.get(&meta.id);
        let result = match cached {
            // The loader we waited for finished in the meantime: its handle is the instance.
            Some(handle) => Ok(handle),
            None => self.load(meta).await,
        };

        // The entry is dropped once the load is over *and* the per-file guard is released; the
        // handle is in the cache by now, so a caller that starts loading afterwards finds it
        // there instead of loading again. Only our own entry is removed: a caller that already
        // replaced it must not lose the load it is currently waiting for.
        drop(guard);
        let mut loading = self.loading.lock().await;
        if loading
            .get(&meta.id)
            .map(|entry| Arc::ptr_eq(entry, &load_lock))
            .unwrap_or(false)
        {
            loading.remove(&meta.id);
        }
        drop(loading);
        result
    }

    async fn load(&self, meta: &FileMetadata) -> Result<DocumentHandle> {
        let room = self.room(&meta.id).await;
        let mut loaded = restore_document(&meta.id, self.store.as_ref()).await?;
        // Callers may hold a stale metadata copy (a previous write can have changed the content
        // type, the sync mode or the version), so the authoritative row is re-read here.
        let meta = match self.files.find_by_id(&meta.id).await? {
            Some(current) => current,
            None => meta.clone(),
        };
        loaded.sync_mode = meta.sync_mode;
        loaded.encoding = meisync_core::content_encoding(meta.content_type.as_deref());
        // The per-document revision is authoritative in SQLite: it is bumped on every write and
        // must survive an unload (§26, monotonic revisions).
        loaded.version = meta.version;
        if let Some(record) = self.ydocs.get(&meta.id).await? {
            if record.update_seq > loaded.update_seq {
                loaded.update_seq = record.update_seq;
            }
            loaded.sync_mode = record.sync_mode;
        }
        // Keep the cached metadata in sync with the authoritative YDoc state (§26).
        loaded.etag = crate::loaded::state_etag(&loaded.full_state());
        loaded.size = loaded.bytes().len() as u64;
        if meta.sync_mode == SyncMode::Full {
            // Full Sync documents only keep their state; drop a stale update log.
            self.store.save_snapshot(&meta.id, &loaded.full_state()).await?;
            self.store.compact(&meta.id, loaded.update_seq).await?;
            loaded.update_count = 0;
            loaded.update_bytes = 0;
        }

        let handle = DocumentHandle::new(
            loaded,
            room,
            self.store.clone(),
            self.ydocs.clone(),
            self.files.clone(),
            self.config.compaction,
        );
        {
            let mut cache = self.cache.lock().await;
            cache.insert(handle.clone());
        }
        debug!(file_id = %meta.id, "ydoc loaded");
        // The document we just loaded must stay the cached instance: evicting it here would
        // hand out a handle that is no longer the cache entry, and the next `open()` would build
        // a second `yrs::Doc` (§24).
        self.enforce_capacity(Some(&meta.id)).await;
        Ok(handle)
    }

    /// Handle if the document happens to be cached, without loading it.
    pub async fn peek(&self, file_id: &FileId) -> Option<DocumentHandle> {
        let cache = self.cache.lock().await;
        cache.entries.get(file_id).cloned()
    }

    /// Drop a document from memory. Every mutation is already persisted, so no flush is needed.
    pub async fn evict(&self, file_id: &FileId) {
        let mut cache = self.cache.lock().await;
        cache.remove(file_id);
    }

    /// Close the room and forget every cached artefact of a deleted file (§28).
    pub async fn close(&self, file_id: &FileId) -> Result<()> {
        let room = self.room(file_id).await;
        room.close();
        self.evict(file_id).await;
        {
            let mut rooms = self.rooms.lock().await;
            rooms.remove(file_id);
        }
        self.store.delete(file_id).await?;
        self.ydocs.delete(file_id).await?;
        Ok(())
    }

    /// Notify YSync clients about a metadata-only change (rename / move) (§29).
    pub async fn notify_moved(&self, file_id: &FileId, new_path: String) {
        let room = self.room(file_id).await;
        room.broadcast(RoomEvent::Moved { path: new_path }, None);
    }

    pub async fn cached_ids(&self) -> Vec<FileId> {
        let cache = self.cache.lock().await;
        cache.entries.keys().cloned().collect()
    }

    pub async fn cached_len(&self) -> usize {
        self.cache.lock().await.entries.len()
    }

    /// Enforce the configured cache capacity by unloading the least recently used documents.
    ///
    /// `protected` is never evicted: it is the document the caller just loaded and is about to
    /// hand out.
    pub async fn enforce_capacity(&self, protected: Option<&FileId>) {
        let max = self.config.max_cached_documents;
        loop {
            let victim = {
                let cache = self.cache.lock().await;
                if cache.entries.len() <= max {
                    None
                } else {
                    let order = cache.lru_order();
                    let mut found = None;
                    for file_id in order {
                        if protected == Some(&file_id) {
                            continue;
                        }
                        if let Some(handle) = cache.entries.get(&file_id) {
                            if handle.room().subscriber_count() == 0 {
                                found = Some(file_id);
                                break;
                            }
                        }
                    }
                    found
                }
            };
            match victim {
                Some(file_id) => {
                    debug!(file_id = %file_id, "evicting ydoc (cache capacity)");
                    self.evict(&file_id).await;
                }
                None => break,
            }
        }
    }

    /// Unload idle documents (no subscribers, untouched for `idle_unload_after`) (§22).
    pub async fn sweep(&self) -> usize {
        let candidates: Vec<(FileId, DocumentHandle)> = {
            let cache = self.cache.lock().await;
            cache
                .lru_order()
                .into_iter()
                .filter_map(|id| cache.entries.get(&id).map(|h| (id, h.clone())))
                .collect()
        };
        let mut evicted = 0;
        for (file_id, handle) in candidates {
            if handle.is_idle_for(self.config.idle_unload_after).await {
                debug!(file_id = %file_id, "unloading idle ydoc");
                self.evict(&file_id).await;
                evicted += 1;
            }
        }
        self.enforce_capacity(None).await;
        evicted
    }

    /// Background cache maintenance.
    pub fn spawn_janitor(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        let interval = self.config.janitor_interval;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                if let Err(err) = tokio::time::timeout(Duration::from_secs(30), manager.sweep()).await {
                warn!("ydoc cache sweep failed: {err}");
            }
            }
        })
    }

    /// Subscribe a YSync client to a document room.
    pub async fn subscribe(
        &self,
        file_id: &FileId,
    ) -> (ConnId, tokio::sync::mpsc::UnboundedReceiver<RoomEvent>) {
        let room = self.room(file_id).await;
        room.subscribe()
    }

    pub async fn unsubscribe(&self, file_id: &FileId, conn: ConnId) {
        let room = self.room(file_id).await;
        room.unsubscribe(conn);
    }

    pub async fn files(&self) -> Arc<FileRepository> {
        self.files.clone()
    }

    /// Diagnostics used by `/api/status`.
    pub async fn status(&self) -> DocumentStatus {
        let cache = self.cache.lock().await;
        DocumentStatus {
            cached_documents: cache.entries.len(),
            rooms: self.rooms.lock().await.len(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentStatus {
    pub cached_documents: usize,
    pub rooms: usize,
}