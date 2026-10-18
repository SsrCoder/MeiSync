use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use meisync_core::{
    content_encoding, decode_content, encode_content, replace_content, ContentEncoding, FileId,
    FileMetadata, MeiSyncError, Preconditions, Result, SyncMode, TransactionOrigin, UserId,
};
use meisync_persistence::fs_store::{CompactionPolicy, DocumentStore, UpdateSeq};
use meisync_persistence::{FileRepository, YDocRepository};
use sha2::{Digest, Sha256};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::room::{Room, RoomEvent};

/// State of one in-memory YDoc (`docs/mvp.md` §21).
pub struct LoadedDocument {
    pub file_id: FileId,
    pub doc: Doc,
    pub sync_mode: SyncMode,
    /// How the bytes are represented inside `Y.Text("content")`.
    pub encoding: ContentEncoding,
    /// Number of updates currently in the on disk log.
    pub update_count: u64,
    pub update_bytes: u64,
    /// Highest persistence sequence applied / written.
    pub update_seq: UpdateSeq,
    /// Sequence covered by the stored snapshot.
    pub snapshot_seq: UpdateSeq,
    /// Cached ETag derived from the YDoc state hash (`docs/mvp.md` §26).
    pub etag: String,
    pub version: i64,
    pub size: u64,
    pub dirty: bool,
    pub last_access: Instant,
}

impl LoadedDocument {
    /// Full state as a Yjs update – used for `Full Sync`, snapshots and ETag computation.
    pub fn full_state(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    }

    pub fn state_vector(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    pub fn diff(&self, sv: &StateVector) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_diff_v1(sv)
    }

    pub fn content(&self) -> String {
        meisync_core::read_text(&self.doc)
    }

    /// Materialised bytes of the file.
    pub fn bytes(&self) -> Vec<u8> {
        decode_content(self.encoding, &self.content())
    }
}

/// Snapshot of the current state as seen by WebDAV / S3.
#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    pub content: Vec<u8>,
    pub etag: String,
    pub version: i64,
    pub size: u64,
    pub sync_mode: SyncMode,
}

/// Handle to exactly one logical YDoc instance for a FileId.
///
/// The handle owns the single `tokio::sync::Mutex` around the document, which enforces
/// `docs/mvp.md` §24: one FileID ⇒ one YDoc instance at any point in time.
#[derive(Clone)]
pub struct DocumentHandle {
    pub file_id: FileId,
    inner: Arc<tokio::sync::Mutex<LoadedDocument>>,
    room: Arc<Room>,
    store: Arc<dyn DocumentStore>,
    ydocs: Arc<YDocRepository>,
    files: Arc<FileRepository>,
    policy: CompactionPolicy,
}

impl DocumentHandle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        loaded: LoadedDocument,
        room: Arc<Room>,
        store: Arc<dyn DocumentStore>,
        ydocs: Arc<YDocRepository>,
        files: Arc<FileRepository>,
        policy: CompactionPolicy,
    ) -> Self {
        room.attach_doc(&loaded.doc);
        Self {
            file_id: loaded.file_id.clone(),
            inner: Arc::new(tokio::sync::Mutex::new(loaded)),
            room,
            store,
            ydocs,
            files,
            policy,
        }
    }

    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    /// Identity of the single in-memory YDoc instance behind this handle.
    pub fn instance_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as *const u8 as usize
    }

    pub async fn subscribers(&self) -> usize {
        self.room.subscriber_count()
    }

    /// A clone of the live YDoc handle (cheap: `yrs::Doc` is reference counted).
    pub async fn raw_doc(&self) -> Doc {
        self.inner.lock().await.doc.clone()
    }

    pub async fn state_vector(&self) -> StateVector {
        self.inner.lock().await.state_vector()
    }

    pub async fn full_state(&self) -> Vec<u8> {
        self.inner.lock().await.full_state()
    }

    pub async fn diff(&self, sv: &StateVector) -> Vec<u8> {
        self.inner.lock().await.diff(sv)
    }

    pub async fn etag(&self) -> String {
        self.inner.lock().await.etag.clone()
    }

    pub async fn version(&self) -> i64 {
        self.inner.lock().await.version
    }

    /// Read the materialised content of the file (`docs/mvp.md` §7 GET / §8 GetObject).
    pub async fn read(&self) -> DocumentSnapshot {
        let mut guard = self.inner.lock().await;
        guard.last_access = Instant::now();
        let content = guard.bytes();
        DocumentSnapshot {
            size: content.len() as u64,
            content,
            etag: guard.etag.clone(),
            version: guard.version,
            sync_mode: guard.sync_mode,
        }
    }

    /// WebDAV PUT / S3 PutObject: replace the whole content with one YDoc transaction.
    pub async fn replace_content(
        &self,
        user: &UserId,
        content: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<DocumentSnapshot> {
        self.replace_content_checked(user, content, content_type, &Preconditions::default())
            .await
    }

    /// Same as [`DocumentHandle::replace_content`], but the `If-Match` / `If-None-Match`
    /// preconditions are evaluated while the document lock is held (§25): two concurrent
    /// conditional writers can never both observe the same ETag and both win.
    pub async fn replace_content_checked(
        &self,
        user: &UserId,
        content: Vec<u8>,
        content_type: Option<String>,
        preconditions: &Preconditions,
    ) -> Result<DocumentSnapshot> {
        let sync_mode = self
            .inner
            .lock()
            .await
            .sync_mode;
        let origin = TransactionOrigin::remote(user, if sync_mode == SyncMode::Full {
            "full-put"
        } else {
            "dav-put"
        });

        let mut guard = self.inner.lock().await;
        let current_etag = guard.etag.clone();
        preconditions.check_write(Some(&current_etag))?;
        guard.last_access = Instant::now();
        if let Some(content_type) = content_type.as_deref() {
            guard.encoding = content_encoding(Some(content_type));
            if guard.sync_mode != SyncMode::Full {
                guard.sync_mode =
                    meisync_core::guess_sync_mode(Some(content_type));
            }
        }
        let text = encode_content(guard.encoding, &content);
        let update = replace_content(&guard.doc, &origin, &text);
        let mut snapshot = finish_mutation(
            &mut guard,
            update.clone(),
            self.store.as_ref(),
            self.ydocs.as_ref(),
            self.policy,
        )
        .await?;
        // Content metadata is a server side cache; it never lives inside the YDoc (§6).
        let version = self
            .files
            .touch_content(
                &self.file_id,
                snapshot.size,
                snapshot.etag.clone(),
                content_type,
                None,
            )
            .await?;
        guard.version = version;
        // Report the revision this write produced, not the one it replaced.
        snapshot.version = version;
        guard.dirty = false;
        drop(guard);
        self.room.broadcast(RoomEvent::Update(update), None);
        Ok(snapshot)
    }

    /// YSync: apply a remote CRDT update, persist it and broadcast it to the other peers.
    pub async fn apply_update(
        &self,
        user: &UserId,
        update: Vec<u8>,
        except: Option<crate::room::ConnId>,
    ) -> Result<DocumentSnapshot> {
        let mut guard = self.inner.lock().await;
        guard.last_access = Instant::now();
        {
            let update = Update::decode_v1(&update)
                .map_err(|e| MeiSyncError::Document(format!("invalid yjs update: {e}")))?;
            let mut txn = guard.doc.transact_mut_with(TransactionOrigin::remote(user, "ysync"));
            txn.apply_update(update)
                .map_err(|e| MeiSyncError::Document(format!("apply update failed: {e}")))?;
        }
        let mut snapshot = finish_mutation(
            &mut guard,
            update.clone(),
            self.store.as_ref(),
            self.ydocs.as_ref(),
            self.policy,
        )
        .await?;
        let version = self
            .files
            .touch_content(
                &self.file_id,
                snapshot.size,
                snapshot.etag.clone(),
                None,
                None,
            )
            .await?;
        guard.version = version;
        // Report the revision this update produced, not the one it replaced.
        snapshot.version = version;
        guard.dirty = false;
        drop(guard);
        self.room.broadcast(RoomEvent::Update(update), except);
        Ok(snapshot)
    }

    /// Write a snapshot of the current state (used before eviction and on shutdown).
    pub async fn checkpoint(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        let state = guard.full_state();
        self.store.save_snapshot(&self.file_id, &state).await?;
        let seq = guard.update_seq;
        self.ydocs.record_snapshot(&self.file_id, seq).await?;
        self.store.compact(&self.file_id, seq).await?;
        guard.snapshot_seq = seq;
        guard.update_count = 0;
        guard.update_bytes = 0;
        guard.dirty = false;
        Ok(())
    }

    pub async fn is_idle_for(&self, ttl: std::time::Duration) -> bool {
        let guard = self.inner.lock().await;
        self.room.subscriber_count() == 0 && guard.last_access.elapsed() > ttl
    }

    pub async fn touch(&self) {
        self.inner.lock().await.last_access = Instant::now();
    }
}

/// Persist an update produced by a transaction and refresh the cached ETag / size.
async fn finish_mutation(
    guard: &mut LoadedDocument,
    update: Vec<u8>,
    store: &dyn DocumentStore,
    ydocs: &YDocRepository,
    policy: CompactionPolicy,
) -> Result<DocumentSnapshot> {
    let file_id = guard.file_id.clone();
    let full = guard.full_state();
    let etag = state_etag(&full);
    let content = guard.content();
    let bytes = decode_content(guard.encoding, &content);

    if guard.sync_mode == SyncMode::Full {
        // Full Sync: only the complete state is kept, no update log.
        store.save_snapshot(&file_id, &full).await?;
        guard.update_seq += 1;
        guard.snapshot_seq = guard.update_seq;
        ydocs.record_snapshot(&file_id, guard.update_seq).await?;
    } else {
        let seq = store.append_update(&file_id, &update).await?;
        guard.update_seq = seq;
        guard.update_count += 1;
        guard.update_bytes += update.len() as u64;
        ydocs.record_update(&file_id, seq).await?;

        let stats = store.stats(&file_id).await?;
        if policy.needs_compaction(&stats) {
            // Compaction: snapshot + update log -> new snapshot -> drop obsolete updates (§20).
            store.save_snapshot(&file_id, &full).await?;
            ydocs.record_snapshot(&file_id, guard.update_seq).await?;
            store.compact(&file_id, guard.update_seq).await?;
            guard.snapshot_seq = guard.update_seq;
            guard.update_count = 0;
            guard.update_bytes = 0;
        }
    }

    guard.etag = etag.clone();
    guard.size = bytes.len() as u64;
    guard.dirty = true;

    Ok(DocumentSnapshot {
        size: guard.size,
        content: bytes,
        etag,
        version: guard.version,
        sync_mode: guard.sync_mode,
    })
}

/// ETag = hash of the current YDoc state (`docs/mvp.md` §26).
pub fn state_etag(state: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state);
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

/// Restore a YDoc from `snapshot + updates` (`docs/mvp.md` §17, §18).
pub async fn restore_document(
    file_id: &FileId,
    store: &dyn DocumentStore,
) -> Result<LoadedDocument> {
    let doc = Doc::new();
    let snapshot_seq = 0u64;
    if let Some(snapshot) = store.load_snapshot(file_id).await? {
        let update = Update::decode_v1(&snapshot).map_err(|e| {
            MeiSyncError::Document(format!("corrupt snapshot for {file_id}: {e}"))
        })?;
        let mut txn = doc.transact_mut_with(TransactionOrigin::system("restore"));
        txn.apply_update(update)
            .map_err(|e| MeiSyncError::Document(format!("apply snapshot failed: {e}")))?;
    }

    let updates = store.load_updates(file_id, 0).await?;
    let mut last_seq = snapshot_seq;
    let mut count = 0u64;
    let mut bytes = 0u64;
    for (seq, raw) in updates {
        let update = Update::decode_v1(&raw).map_err(|e| {
            MeiSyncError::Document(format!("corrupt update {seq} for {file_id}: {e}"))
        })?;
        let mut txn = doc.transact_mut_with(TransactionOrigin::system("restore"));
        txn.apply_update(update).map_err(|e| {
            MeiSyncError::Document(format!("apply update {seq} failed for {file_id}: {e}"))
        })?;
        last_seq = seq;
        count += 1;
        bytes += raw.len() as u64;
    }

    let state = {
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    };
    let etag = state_etag(&state);
    let size = meisync_core::read_text(&doc).len() as u64;

    Ok(LoadedDocument {
        file_id: file_id.clone(),
        doc,
        encoding: ContentEncoding::Text,
        sync_mode: SyncMode::Incremental,
        update_count: count,
        update_bytes: bytes,
        update_seq: last_seq,
        snapshot_seq,
        etag,
        version: 0,
        size,
        dirty: false,
        last_access: Instant::now(),
    })
}

/// Create a brand new, empty YDoc for a freshly created file.
pub fn empty_document(file_id: &FileId, meta: &FileMetadata) -> LoadedDocument {
    let doc = Doc::new();
    let content = meisync_core::read_text(&doc);
    LoadedDocument {
        file_id: file_id.clone(),
        doc,
        encoding: content_encoding(meta.content_type.as_deref()),
        sync_mode: meta.sync_mode,
        update_count: 0,
        update_bytes: 0,
        update_seq: 0,
        snapshot_seq: 0,
        etag: state_etag(&[0, 0]),
        version: 0,
        size: content.len() as u64,
        dirty: false,
        last_access: Instant::now(),
    }
}

/// Ensure the root text type exists so materialised content is always defined.
pub fn prime_text(doc: &Doc, text: &str) {
    if text.is_empty() {
        return;
    }
    replace_content(doc, &TransactionOrigin::system("init"), text);
}

/// Persist the very first state of a new document, so the file exists even before the first write.
pub async fn persist_initial(
    handle: &DocumentHandle,
) -> Result<()> {
    handle.checkpoint().await
}

/// Utility used by tests and diagnostics.
pub fn encode_update(update: &Update) -> Vec<u8> {
    update.encode_v1()
}

/// Timestamp helper kept next to the document layer for diagnostics.
pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}