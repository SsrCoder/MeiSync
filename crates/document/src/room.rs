//! Room / broadcast layer (`docs/mvp.md` §9, §10, §21, §34).
//!
//! One FileDocument corresponds to exactly one logical room. YSync clients subscribe to it and
//! receive every CRDT update, awareness update and lifecycle event (rename / move / delete) that
//! happens on the document. Awareness is pure runtime state: it is never persisted with the
//! YDoc, it lives and dies with the WebSocket connections of the room.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use meisync_core::FileId;
use tokio::sync::mpsc;
use yrs::block::ClientID;
use yrs::sync::Awareness;
use yrs::updates::encoder::Encode;

/// Identifier of a single YSync connection inside a room.
pub type ConnId = u64;

/// Everything a subscribed client can receive.
#[derive(Debug, Clone)]
pub enum RoomEvent {
    /// A Yjs/Yrs update produced by another peer (or by a WebDAV/S3 write).
    Update(Vec<u8>),
    /// Raw awareness payload (`AwarenessUpdate` encoding) to relay.
    Awareness(Vec<u8>),
    /// The document was deleted.
    Deleted,
    /// Metadata only change (rename / move / visibility) – `path` is the new namespace path.
    Moved { path: String },
}

impl RoomEvent {
    pub fn is_control(&self) -> bool {
        !matches!(self, RoomEvent::Update(_) | RoomEvent::Awareness(_))
    }
}

#[derive(Default)]
pub struct Subscriber {
    tx: Option<mpsc::UnboundedSender<RoomEvent>>,
    client_ids: Mutex<HashSet<ClientID>>,
}

/// Per document broadcast room.
pub struct Room {
    file_id: FileId,
    subscribers: Mutex<HashMap<ConnId, Subscriber>>,
    awareness: Mutex<Option<Awareness>>,
    next_conn: AtomicU64,
}

impl Room {
    pub fn new(file_id: FileId) -> Arc<Self> {
        Arc::new(Self {
            file_id,
            subscribers: Mutex::new(HashMap::new()),
            awareness: Mutex::new(None),
            next_conn: AtomicU64::new(1),
        })
    }

    pub fn file_id(&self) -> &FileId {
        &self.file_id
    }

    /// Bind the room to the YDoc so awareness queries can read the state vector.
    pub fn attach_doc(&self, doc: &yrs::Doc) {
        let mut guard = self.awareness.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Awareness::new(doc.clone()));
        }
    }

    pub fn with_awareness<R>(&self, f: impl FnOnce(&mut Awareness) -> R) -> Option<R> {
        let mut guard = self.awareness.lock().unwrap();
        guard.as_mut().map(f)
    }

    pub fn subscribe(self: &Arc<Self>) -> (ConnId, mpsc::UnboundedReceiver<RoomEvent>) {
        let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap()
            .insert(id, Subscriber::new(tx));
        (id, rx)
    }

    /// Remove a connection from the room and tell the remaining peers that the awareness
    /// states it introduced are gone (§10).
    pub fn unsubscribe(&self, conn: ConnId) {
        let removed = self.subscribers.lock().unwrap().remove(&conn);
        let Some(sub) = removed else { return };
        let clients: Vec<ClientID> = sub.client_ids.lock().unwrap().iter().copied().collect();
        if clients.is_empty() {
            return;
        }
        let update = {
            let mut guard = self.awareness.lock().unwrap();
            let Some(awareness) = guard.as_mut() else {
                return;
            };
            for client in &clients {
                awareness.remove_state(*client);
            }
            // `remove_state` marks the client as disconnected (`null` payload, bumped clock),
            // so the encoded update is exactly the removal the other peers have to apply.
            awareness.update_with_clients(clients.iter().copied()).ok()
        };
        if let Some(update) = update {
            self.broadcast(RoomEvent::Awareness(update.encode_v1()), Some(conn));
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }

    /// Remember which Yjs client ids a connection introduced, so they can be removed on
    /// disconnect.
    pub fn note_client(&self, conn: ConnId, client_id: ClientID) {
        if let Some(sub) = self.subscribers.lock().unwrap().get(&conn) {
            sub.client_ids.lock().unwrap().insert(client_id);
        }
    }

    pub fn broadcast(&self, event: RoomEvent, except: Option<ConnId>) {
        let subs = self.subscribers.lock().unwrap();
        for (id, sub) in subs.iter() {
            if Some(*id) == except {
                continue;
            }
            if let Some(tx) = &sub.tx {
                let _ = tx.send(event.clone());
            }
        }
    }

    /// Mark the room as deleted and drop every subscriber (each client observes the closure of
    /// the stream and learns that the file is gone).
    pub fn close(&self) {
        let mut subs = self.subscribers.lock().unwrap();
        for (_, sub) in subs.drain() {
            if let Some(tx) = &sub.tx {
                let _ = tx.send(RoomEvent::Deleted);
            }
        }
        let mut guard = self.awareness.lock().unwrap();
        *guard = None;
    }
}

impl Subscriber {
    fn new(tx: mpsc::UnboundedSender<RoomEvent>) -> Self {
        Self {
            tx: Some(tx),
            client_ids: Mutex::new(HashSet::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_reaches_all_except_sender() {
        let room = Room::new(FileId::new("f"));
        let (a, mut ra) = room.subscribe();
        let (_b, mut rb) = room.subscribe();
        room.broadcast(RoomEvent::Update(vec![1, 2, 3]), Some(a));
        assert!(ra.try_recv().is_err(), "sender must not receive its own update");
        assert!(matches!(rb.recv().await, Some(RoomEvent::Update(v)) if v == vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn close_notifies_subscribers() {
        let room = Room::new(FileId::new("f"));
        let (_a, mut rx) = room.subscribe();
        room.close();
        assert!(matches!(rx.recv().await, Some(RoomEvent::Deleted)));
    }
}