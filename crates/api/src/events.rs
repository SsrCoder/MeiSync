//! Change notifications for sync clients and the admin console.
//!
//! Every mutation goes through `FileService`, so this is the one place where a change has to be
//! announced. The hub deliberately carries *metadata only*: consumers fetch the materialised
//! bytes through the normal file API, and a slow consumer can always recover by listing the
//! manifest again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// What happened to a namespace entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Updated,
    Deleted,
    Moved,
}

/// One observable mutation. `seq` is a process-local monotonic counter; it is intentionally not
/// durable because a disconnected client falls back to a manifest reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub seq: u64,
    pub kind: ChangeKind,
    pub file_id: String,
    /// Current path (for `deleted`, the path the entry had before it disappeared).
    pub path: String,
    /// Previous path, only set for `moved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub size: u64,
    pub collection: bool,
    pub updated_at: DateTime<Utc>,
    /// Account that performed the write.
    pub actor_id: String,
}

impl FileChange {
    /// Whether a subscriber rooted at `root` is interested in this change.
    pub fn touches(&self, root: &str) -> bool {
        let under = |path: &str| {
            path == root
                || path
                    .strip_prefix(root)
                    .map(|rest| rest.starts_with('/'))
                    .unwrap_or(false)
        };
        under(&self.path) || self.old_path.as_deref().map(under).unwrap_or(false)
    }
}

/// Broadcast fan-out used by the WebSocket sync endpoint (`GET /api/sync/ws`).
#[derive(Clone)]
pub struct ChangeHub {
    tx: broadcast::Sender<FileChange>,
    seq: Arc<AtomicU64>,
}

impl ChangeHub {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self {
            tx,
            seq: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FileChange> {
        self.tx.subscribe()
    }

    pub fn publish(&self, mut change: FileChange) {
        change.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        // No subscribers is normal (a quiet node); the send error is irrelevant.
        let _ = self.tx.send(change);
    }

    pub fn last_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed).saturating_sub(1)
    }
}

impl Default for ChangeHub {
    fn default() -> Self {
        Self::new(1024)
    }
}
