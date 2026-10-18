use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meisync_core::{FileId, MeiSyncError, Result};

/// Server side persistence sequence (`docs/mvp.md` §19).
///
/// This is *not* the Yjs logical clock: it only orders the update log on disk.
pub type UpdateSeq = u64;

pub const DOCS_DIR_NAME: &str = "documents";
pub const SNAPSHOT_FILE: &str = "snapshot";
pub const UPDATES_DIR_NAME: &str = "updates";

/// Compaction thresholds (`docs/mvp.md` §20) – configurable, defaults follow the design doc.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    pub max_updates: u64,
    pub max_update_bytes: u64,
    pub snapshot_bytes: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            max_updates: 1000,
            max_update_bytes: 4 * 1024 * 1024,
            snapshot_bytes: 8 * 1024 * 1024,
        }
    }
}

impl CompactionPolicy {
    pub fn needs_compaction(&self, stats: &StoreStats) -> bool {
        stats.update_count >= self.max_updates
            || stats.update_bytes >= self.max_update_bytes
            || stats.snapshot_bytes >= self.snapshot_bytes
    }
}

#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    pub snapshot_bytes: u64,
    pub update_count: u64,
    pub update_bytes: u64,
    pub last_seq: UpdateSeq,
}

/// Storage abstraction (`docs/mvp.md` §32).
///
/// MVP implements it as [`LocalDocumentStore`] (filesystem + SQLite metadata); a future
/// PostgresDocumentStore / S3DocumentStore can be dropped in without touching the layers above.
#[async_trait]
pub trait DocumentStore: Send + Sync {
    /// Full YDoc state (`encode_state_as_update_v1`) if a snapshot exists.
    async fn load_snapshot(&self, file_id: &FileId) -> Result<Option<Vec<u8>>>;

    /// All update log entries with `seq > after_seq`, ordered by sequence.
    async fn load_updates(&self, file_id: &FileId, after_seq: UpdateSeq)
        -> Result<Vec<(UpdateSeq, Vec<u8>)>>;

    /// Append one update, returning its assigned server side sequence.
    async fn append_update(&self, file_id: &FileId, update: &[u8]) -> Result<UpdateSeq>;

    /// Persist a snapshot covering everything up to the current sequence.
    async fn save_snapshot(&self, file_id: &FileId, snapshot: &[u8]) -> Result<()>;

    /// Delete obsolete updates after a snapshot was written.
    async fn compact(&self, file_id: &FileId, through_seq: UpdateSeq) -> Result<()>;

    async fn stats(&self, file_id: &FileId) -> Result<StoreStats>;

    /// Remove every persisted artefact of a document.
    async fn delete(&self, file_id: &FileId) -> Result<()>;
}

/// Filesystem backed [`DocumentStore`] (`docs/mvp.md` §16):
///
/// ```text
/// data/documents/<file-id>/snapshot
/// data/documents/<file-id>/updates/00000001
/// ```
pub struct LocalDocumentStore {
    root: PathBuf,
    seqs: Mutex<HashMap<String, UpdateSeq>>,
    policy: CompactionPolicy,
}

impl LocalDocumentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            seqs: Mutex::new(HashMap::new()),
            policy: CompactionPolicy::default(),
        }
    }

    pub fn with_policy(root: impl Into<PathBuf>, policy: CompactionPolicy) -> Self {
        Self {
            root: root.into(),
            seqs: Mutex::new(HashMap::new()),
            policy,
        }
    }

    pub fn shared(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self::new(root))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn policy(&self) -> CompactionPolicy {
        self.policy
    }

    pub fn doc_dir(&self, file_id: &FileId) -> PathBuf {
        self.root.join(file_id.as_str())
    }

    pub fn snapshot_path(&self, file_id: &FileId) -> PathBuf {
        self.doc_dir(file_id).join(SNAPSHOT_FILE)
    }

    pub fn updates_dir(&self, file_id: &FileId) -> PathBuf {
        self.doc_dir(file_id).join(UPDATES_DIR_NAME)
    }

    pub fn update_path(&self, file_id: &FileId, seq: UpdateSeq) -> PathBuf {
        self.updates_dir(file_id).join(format!("{seq:08}"))
    }

    /// Highest sequence present on disk.
    fn scan_last_seq(&self, file_id: &FileId) -> UpdateSeq {
        let dir = self.updates_dir(file_id);
        let mut last = 0;
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if let Ok(seq) = name.parse::<u64>() {
                        last = last.max(seq);
                    }
                }
            }
        }
        last
    }

    fn next_seq(&self, file_id: &FileId) -> UpdateSeq {
        let mut map = self.seqs.lock().unwrap();
        let entry = map.entry(file_id.0.clone()).or_insert_with(|| 0);
        *entry += 1;
        *entry
    }

    fn note_seq(&self, file_id: &FileId, seq: UpdateSeq) {
        let mut map = self.seqs.lock().unwrap();
        let entry = map.entry(file_id.0.clone()).or_insert(0);
        if *entry < seq {
            *entry = seq;
        }
    }

    fn last_seq(&self, file_id: &FileId) -> UpdateSeq {
        let mut map = self.seqs.lock().unwrap();
        let scanned = self.scan_last_seq(file_id);
        let entry = map.entry(file_id.0.clone()).or_insert(0);
        if *entry < scanned {
            *entry = scanned;
        }
        *entry
    }
}

fn storage<E: std::fmt::Display>(err: E) -> MeiSyncError {
    MeiSyncError::Storage(err.to_string())
}

/// Write atomically: temp file in the same directory + rename.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "tmp{}",
        std::process::id() as u64 ^ rand_suffix()
    ));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl DocumentStore for LocalDocumentStore {
    async fn load_snapshot(&self, file_id: &FileId) -> Result<Option<Vec<u8>>> {
        let path = self.snapshot_path(file_id);
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(storage(err)),
        })
        .await
        .map_err(storage)?
    }

    async fn load_updates(
        &self,
        file_id: &FileId,
        after_seq: UpdateSeq,
    ) -> Result<Vec<(UpdateSeq, Vec<u8>)>> {
        let dir = self.updates_dir(file_id);
        let id = file_id.clone();
        self.note_seq(file_id, self.last_seq(file_id));
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
                Err(err) => return Err(storage(err)),
            };
            let mut names: Vec<(u64, PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };
                if let Ok(seq) = name.parse::<u64>() {
                    if seq > after_seq {
                        names.push((seq, path));
                    }
                }
            }
            names.sort_by_key(|(seq, _)| *seq);
            for (seq, path) in names {
                let bytes = fs::read(&path)?;
                out.push((seq, bytes));
            }
            let _ = &id;
            Ok(out)
        })
        .await
        .map_err(storage)?
    }

    async fn append_update(&self, file_id: &FileId, update: &[u8]) -> Result<UpdateSeq> {
        let seq = self.next_seq(file_id);
        let path = self.update_path(file_id, seq);
        let data = update.to_vec();
        tokio::task::spawn_blocking(move || write_atomic(&path, &data))
            .await
            .map_err(storage)??;
        Ok(seq)
    }

    async fn save_snapshot(&self, file_id: &FileId, snapshot: &[u8]) -> Result<()> {
        let path = self.snapshot_path(file_id);
        let data = snapshot.to_vec();
        tokio::task::spawn_blocking(move || write_atomic(&path, &data))
            .await
            .map_err(storage)??;
        Ok(())
    }

    async fn compact(&self, file_id: &FileId, through_seq: UpdateSeq) -> Result<()> {
        let dir = self.updates_dir(file_id);
        tokio::task::spawn_blocking(move || {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(storage(err)),
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };
                if let Ok(seq) = name.parse::<u64>() {
                    if seq <= through_seq {
                        fs::remove_file(path)?;
                    }
                }
            }
            Ok::<(), MeiSyncError>(())
        })
        .await
        .map_err(storage)??;
        self.note_seq(file_id, through_seq);
        Ok(())
    }

    async fn stats(&self, file_id: &FileId) -> Result<StoreStats> {
        let snapshot = self.snapshot_path(file_id);
        let dir = self.updates_dir(file_id);
        let last = self.last_seq(file_id);
        tokio::task::spawn_blocking(move || {
            let snapshot_bytes = fs::metadata(&snapshot).map(|m| m.len()).unwrap_or(0);
            let mut update_count = 0u64;
            let mut update_bytes = 0u64;
            let mut max_seq = 0u64;
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.parse::<u64>().is_ok() {
                            update_count += 1;
                            update_bytes += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            if let Ok(seq) = name.parse::<u64>() {
                                max_seq = max_seq.max(seq);
                            }
                        }
                    }
                }
            }
            Ok(StoreStats {
                snapshot_bytes,
                update_count,
                update_bytes,
                last_seq: max_seq,
            })
        })
        .await
        .map_err(storage)?
        .map(|mut stats| {
            stats.last_seq = stats.last_seq.max(last);
            stats
        })
    }

    async fn delete(&self, file_id: &FileId) -> Result<()> {
        let dir = self.doc_dir(file_id);
        {
            let mut map = self.seqs.lock().unwrap();
            map.remove(&file_id.0);
        }
        tokio::task::spawn_blocking(move || match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(storage(err)),
        })
        .await
        .map_err(storage)??;
        Ok(())
    }
}