//! DocumentManager tests: single YDoc per file, lifecycle, cache and persistence
//! (`docs/mvp.md` §21 – §24).

use std::sync::Arc;

use chrono::Utc;
use meisync_core::{FileId, FileKind, FileMetadata, SyncMode, UserId, Visibility};
use meisync_document::{DocumentManager, DocumentManagerConfig, RoomEvent};
use meisync_persistence::fs_store::{CompactionPolicy, DocumentStore, LocalDocumentStore};
use meisync_persistence::{FileRepository, SqliteDatabase, YDocRepository};
use tempfile::TempDir;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{StateVector, Transact, Update};

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text.as_bytes())
        .expect("base64")
}

struct Fixture {
    manager: Arc<DocumentManager>,
    db: Arc<SqliteDatabase>,
    store: Arc<LocalDocumentStore>,
    meta: FileMetadata,
    _dir: TempDir,
}

async fn fixture() -> Fixture {
    fixture_with(DocumentManagerConfig::default()).await
}

async fn fixture_with(config: DocumentManagerConfig) -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let db = Arc::new(SqliteDatabase::open_in_memory().expect("db"));
    let store = Arc::new(LocalDocumentStore::new(dir.path()));
    let files = Arc::new(FileRepository::new(db.clone()));
    let ydocs = Arc::new(YDocRepository::new(db.clone()));

    let now = Utc::now();
    let meta = FileMetadata {
        id: FileId::generate(),
        parent_id: None,
        name: "notes.md".into(),
        owner_id: UserId::new("alice"),
        visibility: Visibility::Private,
        content_type: Some("text/markdown".into()),
        kind: FileKind::File,
        created_at: now,
        updated_at: now,
        size: 0,
        etag: None,
        version: 0,
        sync_mode: SyncMode::Incremental,
    };
    let meta = files.insert(meta).await.expect("insert");

    let manager = DocumentManager::new(store.clone(), files, ydocs, config);
    Fixture {
        manager,
        db,
        store,
        meta,
        _dir: dir,
    }
}

#[tokio::test]
async fn concurrent_cold_opens_share_one_ydoc_instance() {
    let f = fixture().await;
    let user = UserId::new("alice");

    // Many callers open the same cold file at the same time: the load has to be single flight,
    // otherwise every caller builds its own `yrs::Doc` and the last insert orphans the others
    // (§24: one FileID ⇒ one YDoc instance).
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let manager = f.manager.clone();
        let meta = f.meta.clone();
        tasks.push(tokio::spawn(async move {
            manager.open(&meta).await.expect("open").instance_id()
        }));
    }
    let mut instances = Vec::new();
    for task in tasks {
        instances.push(task.await.expect("join"));
    }
    assert!(
        instances.windows(2).all(|pair| pair[0] == pair[1]),
        "every caller must share one YDoc, got {instances:?}"
    );
    assert_eq!(f.manager.cached_len().await, 1);

    // A write through one handle is visible through any other handle.
    let handle = f.manager.open(&f.meta).await.expect("open");
    handle
        .replace_content(&user, b"single flight".to_vec(), None)
        .await
        .expect("write");
    assert_eq!(handle.read().await.content, b"single flight".to_vec());
}

#[tokio::test]
async fn one_ydoc_per_file_is_shared_by_all_callers() {
    let f = fixture().await;
    let user = UserId::new("alice");

    let first = f.manager.open(&f.meta).await.expect("open");
    let second = f.manager.open(&f.meta).await.expect("open");
    // Two independent opens must resolve to the very same in-memory instance (§24).
    assert_eq!(first.instance_id(), second.instance_id());

    first
        .replace_content(&user, b"hello".to_vec(), None)
        .await
        .expect("write");
    let snapshot = second.read().await;
    assert_eq!(snapshot.content, b"hello".to_vec());
}

#[tokio::test]
async fn etag_changes_with_every_transaction() {
    let f = fixture().await;
    let user = UserId::new("alice");
    let handle = f.manager.open(&f.meta).await.expect("open");

    let before = handle.read().await;
    let first = handle
        .replace_content(&user, b"one".to_vec(), None)
        .await
        .expect("write");
    assert_ne!(before.etag, first.etag);
    let second = handle
        .replace_content(&user, b"two".to_vec(), None)
        .await
        .expect("write");
    assert_ne!(first.etag, second.etag);
    assert_eq!(second.size, 3);
}

#[tokio::test]
async fn updates_are_persisted_and_replayed_after_eviction() {
    let f = fixture().await;
    let user = UserId::new("alice");
    {
        let handle = f.manager.open(&f.meta).await.expect("open");
        for index in 0..5 {
            handle
                .replace_content(&user, format!("v{index}").into_bytes(), None)
                .await
                .expect("write");
        }
    }
    // The update log holds every transaction (§18).
    let updates = f.store.load_updates(&f.meta.id, 0).await.expect("load");
    assert_eq!(updates.len(), 5);

    f.manager.evict(&f.meta.id).await;
    let handle = f.manager.open(&f.meta).await.expect("reopen");
    let snapshot = handle.read().await;
    assert_eq!(snapshot.content, b"v4".to_vec());
}

#[tokio::test]
async fn full_state_sync_documents_keep_no_update_log() {
    let f = fixture_with(DocumentManagerConfig::default()).await;
    let mut meta = f.meta.clone();
    meta.sync_mode = SyncMode::Full;
    let meta = meta.clone();
    FileRepository::new(f.db.clone())
        .update_meta(&meta)
        .await
        .expect("update meta");
    let user = UserId::new("alice");

    let handle = f.manager.open(&meta).await.expect("open");
    handle
        .replace_content(&user, b"first".to_vec(), Some("application/octet-stream".into()))
        .await
        .expect("write");
    handle
        .replace_content(&user, b"second".to_vec(), Some("application/octet-stream".into()))
        .await
        .expect("write");

    let stats = f.store.stats(&meta.id).await.expect("stats");
    assert_eq!(stats.update_count, 0, "full sync keeps no update log");
    assert!(stats.snapshot_bytes > 0);

    f.manager.evict(&meta.id).await;
    let handle = f.manager.open(&meta).await.expect("reopen");
    assert_eq!(handle.read().await.content, b"second".to_vec());
}

#[tokio::test]
async fn compaction_runs_when_the_log_grows() {
    let f = fixture_with(DocumentManagerConfig {
        compaction: CompactionPolicy {
            max_updates: 3,
            max_update_bytes: 1 << 30,
            snapshot_bytes: 1 << 30,
        },
        ..Default::default()
    })
    .await;
    let user = UserId::new("alice");
    let handle = f.manager.open(&f.meta).await.expect("open");
    for index in 0..10 {
        handle
            .replace_content(&user, format!("v{index}").into_bytes(), None)
            .await
            .expect("write");
    }
    let stats = f.store.stats(&f.meta.id).await.expect("stats");
    assert!(stats.update_count <= 3, "update log was compacted");

    f.manager.evict(&f.meta.id).await;
    let handle = f.manager.open(&f.meta).await.expect("reopen");
    assert_eq!(handle.read().await.content, b"v9".to_vec());
}

#[tokio::test]
async fn remote_updates_broadcast_to_the_room() {
    let f = fixture().await;
    let handle = f.manager.open(&f.meta).await.expect("open");
    let (conn, mut events) = f.manager.subscribe(&f.meta.id).await;

    // A WebDAV style write reaches every YSync client of the room (§34).
    handle
        .replace_content(&UserId::new("alice"), b"broadcast me".to_vec(), None)
        .await
        .expect("write");
    match events.recv().await.expect("event") {
        RoomEvent::Update(bytes) => {
            let update = Update::decode_v1(&bytes).expect("update");
            assert!(!update.is_empty());
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // A client originating the update must not receive its own echo.
    let update = {
        let doc = handle.full_state().await;
        Update::decode_v1(&doc).expect("update")
    };
    handle
        .apply_update(&UserId::new("alice"), update.encode_v1(), Some(conn))
        .await
        .expect("apply");
    assert!(events.try_recv().is_err(), "sender must not receive its own update");

    f.manager.unsubscribe(&f.meta.id, conn).await;
}

#[tokio::test]
async fn diff_and_state_vector_are_incremental() {
    let f = fixture().await;
    let handle = f.manager.open(&f.meta).await.expect("open");
    handle
        .replace_content(&UserId::new("alice"), b"a".repeat(2048), None)
        .await
        .expect("write");

    let sv = handle.state_vector().await;
    let empty = handle.diff(&StateVector::default()).await;
    let current = handle.diff(&sv).await;
    assert!(empty.len() > current.len());
    // Nothing missing -> only the empty header.
    assert!(current.len() <= 3, "unexpected diff size {}", current.len());
}

#[tokio::test]
async fn binary_content_roundtrips_through_the_ydoc() {
    let f = fixture().await;
    let handle = f.manager.open(&f.meta).await.expect("open");
    let payload = vec![0u8, 159, 146, 150, 255, 0, 17];
    handle
        .replace_content(
            &UserId::new("alice"),
            payload.clone(),
            Some("application/octet-stream".into()),
        )
        .await
        .expect("write");
    assert_eq!(handle.read().await.content, payload);

    // The YDoc still holds a single text root type, base64 encoded (§6 + §5.2).
    let state = handle.full_state().await;
    let mirror = yrs::Doc::new();
    {
        let mut txn = mirror.transact_mut();
        txn.apply_update(Update::decode_v1(&state).expect("update"))
            .expect("apply");
    }
    let text = meisync_core::read_text(&mirror);
    assert_eq!(
        base64_decode(&text),
        payload,
        "the YDoc text holds the base64 encoded bytes"
    );
}

#[tokio::test]
async fn sweep_unloads_idle_documents() {
    let f = fixture_with(DocumentManagerConfig {
        idle_unload_after: std::time::Duration::from_millis(0),
        ..Default::default()
    })
    .await;
    let handle = f.manager.open(&f.meta).await.expect("open");
    handle
        .replace_content(&UserId::new("alice"), b"x".to_vec(), None)
        .await
        .expect("write");
    assert_eq!(f.manager.cached_len().await, 1);

    let evicted = f.manager.sweep().await;
    assert_eq!(evicted, 1);
    assert_eq!(f.manager.cached_len().await, 0);

    // Content survives the unload (§22).
    let handle = f.manager.open(&f.meta).await.expect("reopen");
    assert_eq!(handle.read().await.content, b"x".to_vec());
}

#[tokio::test]
async fn subscribed_documents_are_never_swept() {
    let f = fixture_with(DocumentManagerConfig {
        idle_unload_after: std::time::Duration::from_millis(0),
        ..Default::default()
    })
    .await;
    let _handle = f.manager.open(&f.meta).await.expect("open");
    let (_conn, _events) = f.manager.subscribe(&f.meta.id).await;

    assert_eq!(f.manager.sweep().await, 0);
    assert_eq!(f.manager.cached_len().await, 1);
}

#[tokio::test]
async fn capacity_never_evicts_the_document_that_was_just_opened() {
    let f = fixture_with(DocumentManagerConfig {
        max_cached_documents: 1,
        ..Default::default()
    })
    .await;
    let files = FileRepository::new(f.db.clone());
    let older = files
        .insert(FileMetadata {
            id: FileId::generate(),
            name: "older.md".into(),
            ..f.meta.clone()
        })
        .await
        .expect("insert older");
    let newer = files
        .insert(FileMetadata {
            id: FileId::generate(),
            name: "newer.md".into(),
            ..f.meta.clone()
        })
        .await
        .expect("insert newer");

    // The older document holds a subscriber, so the cache capacity can only be met by evicting
    // the document that is being opened right now.
    let older_handle = f.manager.open(&older).await.expect("open older");
    let (_conn, _events) = f.manager.subscribe(&older.id).await;

    let first = f.manager.open(&newer).await.expect("open newer");
    let second = f.manager.open(&newer).await.expect("open newer");
    assert_eq!(
        first.instance_id(),
        second.instance_id(),
        "a second open must yield the instance the first one loaded"
    );
    assert_eq!(
        f.manager
            .peek(&newer.id)
            .await
            .expect("the just-opened document stays cached")
            .instance_id(),
        first.instance_id()
    );
    assert_eq!(
        f.manager.peek(&older.id).await.expect("older").instance_id(),
        older_handle.instance_id()
    );
}

#[tokio::test]
async fn deleting_a_document_removes_every_artefact() {
    let f = fixture().await;
    let handle = f.manager.open(&f.meta).await.expect("open");
    handle
        .replace_content(&UserId::new("alice"), b"bye".to_vec(), None)
        .await
        .expect("write");
    let (_conn, mut events) = f.manager.subscribe(&f.meta.id).await;

    f.manager.close(&f.meta.id).await.expect("close");

    // Connected clients learn that the file is gone (§28).
    assert!(matches!(events.recv().await, Some(RoomEvent::Deleted)));
    assert!(f.store.load_snapshot(&f.meta.id).await.expect("snapshot").is_none());
    assert_eq!(f.manager.cached_len().await, 0);
    assert!(f.manager.peek(&f.meta.id).await.is_none());
}