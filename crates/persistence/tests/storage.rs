//! Persistence tests: filesystem document store, SQLite metadata repositories.

use std::sync::Arc;

use chrono::Utc;
use meisync_core::{
    FileId, FileKind, FileMetadata, PermissionSet, SyncMode, User, UserId, Visibility,
};
use meisync_persistence::fs_store::{CompactionPolicy, DocumentStore, LocalDocumentStore};
use meisync_persistence::{
    AclProvider, AclRepository, BucketRepository, FileRepository, SessionRepository,
    SqliteDatabase, UserRepository, YDocRepository,
};
use tempfile::TempDir;

fn metadata(name: &str, owner: &str) -> FileMetadata {
    let now = Utc::now();
    FileMetadata {
        id: FileId::generate(),
        parent_id: None,
        name: name.to_string(),
        owner_id: UserId::new(owner),
        visibility: Visibility::Private,
        content_type: Some("text/markdown".into()),
        kind: FileKind::File,
        created_at: now,
        updated_at: now,
        size: 0,
        etag: None,
        version: 0,
        sync_mode: SyncMode::Incremental,
    }
}

// -------------------------------------------------------------------------------------------
// LocalDocumentStore (§16 – §20)
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn updates_are_appended_and_reloaded_in_order() {
    let dir = TempDir::new().expect("temp dir");
    let store = LocalDocumentStore::new(dir.path());
    let file_id = FileId::new("doc-1");

    let seq1 = store.append_update(&file_id, b"one").await.expect("append 1");
    let seq2 = store.append_update(&file_id, b"two").await.expect("append 2");
    assert_eq!(seq1, 1);
    assert_eq!(seq2, 2);

    let updates = store.load_updates(&file_id, 0).await.expect("load");
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0], (1, b"one".to_vec()));
    assert_eq!(updates[1], (2, b"two".to_vec()));

    // `after_seq` implements the incremental replay.
    let tail = store.load_updates(&file_id, 1).await.expect("load tail");
    assert_eq!(tail, vec![(2, b"two".to_vec())]);

    assert!(store.load_snapshot(&file_id).await.expect("snapshot").is_none());
    store.save_snapshot(&file_id, b"snap").await.expect("save");
    assert_eq!(
        store.load_snapshot(&file_id).await.expect("load"),
        Some(b"snap".to_vec())
    );
}

#[tokio::test]
async fn compaction_removes_only_covered_updates() {
    let dir = TempDir::new().expect("temp dir");
    let store = LocalDocumentStore::new(dir.path());
    let file_id = FileId::new("doc-2");
    for payload in [b"a", b"b", b"c", b"d", b"e"] {
        store.append_update(&file_id, payload).await.expect("append");
    }
    store.save_snapshot(&file_id, b"state").await.expect("snapshot");

    let before = store.stats(&file_id).await.expect("stats");
    assert_eq!(before.update_count, 5);

    store.compact(&file_id, 3).await.expect("compact");
    let after = store.stats(&file_id).await.expect("stats");
    assert_eq!(after.update_count, 2);
    let updates = store.load_updates(&file_id, 0).await.expect("load");
    assert_eq!(
        updates.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        vec![4, 5]
    );

    store.delete(&file_id).await.expect("delete");
    assert!(store.load_snapshot(&file_id).await.expect("load").is_none());
    assert_eq!(store.stats(&file_id).await.expect("stats").update_count, 0);
}

#[tokio::test]
async fn compaction_policy_triggers_on_thresholds() {
    let policy = CompactionPolicy {
        max_updates: 3,
        max_update_bytes: 10,
        snapshot_bytes: 20,
    };
    let stats = |updates, bytes, snapshot| meisync_persistence::fs_store::StoreStats {
        snapshot_bytes: snapshot,
        update_count: updates,
        update_bytes: bytes,
        last_seq: updates,
    };
    assert!(!policy.needs_compaction(&stats(0, 0, 0)));
    assert!(policy.needs_compaction(&stats(3, 0, 0)));
    assert!(policy.needs_compaction(&stats(0, 10, 0)));
    assert!(policy.needs_compaction(&stats(0, 0, 20)));
}

// -------------------------------------------------------------------------------------------
// SQLite repositories (§15)
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn users_and_sessions_roundtrip() {
    let db = Arc::new(SqliteDatabase::open_in_memory().expect("db"));
    let users = UserRepository::new(db.clone());
    let sessions = SessionRepository::new(db.clone());

    let user = users
        .create(User {
            id: UserId::new("u1"),
            username: "alice".into(),
            password_hash: "$argon2id$hash".into(),
            created_at: Utc::now(),
        })
        .await
        .expect("create");
    assert_eq!(user.username, "alice");
    assert!(users.find_by_username("alice").await.expect("q").is_some());
    assert!(users.find_by_id(&UserId::new("u1")).await.expect("q").is_some());

    // Usernames are unique.
    let duplicate = users
        .create(User {
            id: UserId::new("u2"),
            username: "alice".into(),
            password_hash: "x".into(),
            created_at: Utc::now(),
        })
        .await;
    assert!(duplicate.is_err());

    let now = Utc::now();
    sessions
        .create(meisync_core::Session {
            token: "tok".into(),
            user_id: UserId::new("u1"),
            created_at: now,
            expires_at: now + chrono::Duration::hours(1),
        })
        .await
        .expect("session");
    assert!(sessions.find("tok").await.expect("q").is_some());
    assert_eq!(
        sessions.list_for_user(&UserId::new("u1")).await.expect("list").len(),
        1
    );
    sessions.delete("tok").await.expect("delete");
    assert!(sessions.find("tok").await.expect("q").is_none());
}

#[tokio::test]
async fn file_namespace_operations() {
    let db = Arc::new(SqliteDatabase::open_in_memory().expect("db"));
    let files = FileRepository::new(db.clone());

    let root = metadata("root", "system");
    let root = files.insert(root).await.expect("insert root");
    let mut child = metadata("alice", "alice");
    child.parent_id = Some(root.id.clone());
    let child = files.insert(child).await.expect("insert child");

    // UNIQUE(parent_id, name)
    let mut duplicate = metadata("alice", "alice");
    duplicate.parent_id = Some(root.id.clone());
    assert!(files.insert(duplicate).await.is_err());

    assert_eq!(files.list_children(&root.id).await.expect("list").len(), 1);
    assert!(files.find_child(&root.id, "alice").await.expect("q").is_some());
    assert!(files.find_child(&root.id, "bob").await.expect("q").is_none());

    // Content bookkeeping bumps the version (§26).
    let version = files
        .touch_content(&child.id, 12, "\"etag\"".into(), None, None)
        .await
        .expect("touch");
    assert_eq!(version, 1);
    let meta = files.find_by_id(&child.id).await.expect("q").unwrap();
    assert_eq!(meta.size, 12);
    assert_eq!(meta.etag.as_deref(), Some("\"etag\""));

    // Move / rename only touches metadata (§29).
    let mut moved = meta.clone();
    moved.name = "alice2".into();
    moved.parent_id = Some(root.id.clone());
    files.update_meta(&moved).await.expect("update");
    assert!(files.find_child(&root.id, "alice2").await.expect("q").is_some());
    assert!(files.find_child(&root.id, "alice").await.expect("q").is_none());
    assert_eq!(files.find_by_id(&child.id).await.expect("q").unwrap().etag.as_deref(), Some("\"etag\""));

    // Descendants + cascade delete.
    let mut file = metadata("note.md", "alice");
    file.parent_id = Some(child.id.clone());
    let file = files.insert(file).await.expect("insert file");
    assert_eq!(files.list_descendants(&child.id).await.expect("desc").len(), 1);
    files.delete(&child.id).await.expect("delete");
    assert!(files.find_by_id(&file.id).await.expect("q").is_none());
}

#[tokio::test]
async fn acl_and_ydoc_state() {
    let db = Arc::new(SqliteDatabase::open_in_memory().expect("db"));
    let files = FileRepository::new(db.clone());
    let acl = AclRepository::new(db.clone());
    let ydocs = YDocRepository::new(db.clone());

    let file = files.insert(metadata("shared.md", "alice")).await.expect("insert");
    assert!(acl
        .acl_permissions(&file.id, &UserId::new("bob"))
        .await
        .expect("perm")
        .is_empty());

    acl.acl_set(&file.id, &UserId::new("bob"), PermissionSet::WRITE)
        .await
        .expect("grant");
    assert!(acl
        .acl_permissions(&file.id, &UserId::new("bob"))
        .await
        .expect("perm")
        .has(PermissionSet::WRITE));
    assert_eq!(acl.acl_list(&file.id).await.expect("list").len(), 1);

    // Revoking removes the row.
    acl.acl_set(&file.id, &UserId::new("bob"), PermissionSet::NONE)
        .await
        .expect("revoke");
    assert!(acl.acl_list(&file.id).await.expect("list").is_empty());

    // YDoc persistence metadata (§15, §19).
    let record = ydocs.get(&file.id).await.expect("get").expect("record");
    assert_eq!(record.update_seq, 0);
    ydocs.record_update(&file.id, 7).await.expect("record update");
    ydocs.record_snapshot(&file.id, 7).await.expect("record snapshot");
    let record = ydocs.get(&file.id).await.expect("get").expect("record");
    assert_eq!(record.update_seq, 7);
    assert_eq!(record.snapshot_version, 1);
}

#[tokio::test]
async fn buckets_are_registered_per_collection() {
    let db = Arc::new(SqliteDatabase::open_in_memory().expect("db"));
    let files = FileRepository::new(db.clone());
    let buckets = BucketRepository::new(db.clone());

    let collection = files
        .insert(metadata("alice", "alice"))
        .await
        .expect("insert");
    buckets
        .upsert("alice", &collection.id, Some(&UserId::new("alice")))
        .await
        .expect("upsert");
    assert_eq!(buckets.find("alice").await.expect("find"), Some(collection.id.clone()));

    // Re-registering moves the bucket to the new collection.
    let other = files.insert(metadata("alice2", "alice")).await.expect("insert");
    buckets.upsert("alice", &other.id, None).await.expect("upsert");
    assert_eq!(buckets.find("alice").await.expect("find"), Some(other.id));
    assert_eq!(buckets.list().await.expect("list").len(), 1);
}