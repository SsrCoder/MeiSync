//! Persistence, compaction, cache and concurrency coverage
//! (`docs/mvp.md` §14 – §20, §22 – §24).

mod common;

use axum::http::StatusCode;
use common::{body_string, Harness};
use meisync_api::{AppState, FileService, ServerConfig};
use meisync_core::SyncMode;
use meisync_persistence::fs_store::CompactionPolicy;
use meisync_persistence::YDocRepository;
use std::sync::Arc;

fn documents_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("documents")
}

fn doc_dir(root: &std::path::Path, file_id: &str) -> std::path::PathBuf {
    documents_dir(root).join(file_id)
}

fn update_log_entries(root: &std::path::Path, file_id: &str) -> usize {
    std::fs::read_dir(doc_dir(root, file_id).join("updates"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().parse::<u64>().is_ok())
                .count()
        })
        .unwrap_or(0)
}

fn file_id_of(root: &std::path::Path, name: &str) -> String {
    let entries = std::fs::read_dir(documents_dir(root)).expect("documents dir");
    for entry in entries.flatten() {
        let path = entry.path().join("updates");
        if path.exists() {
            return entry.file_name().to_string_lossy().to_string();
        }
    }
    let _ = name;
    String::new()
}

async fn put_text(state: &Arc<AppState>, token: &str, path: &str, body: &str) {
    let user = state.auth.authenticate(token).await.expect("auth");
    state
        .files
        .replace_content(
            &user,
            path,
            body.as_bytes().to_vec(),
            Some("text/markdown".into()),
        )
        .await
        .expect("put");
}

#[tokio::test]
async fn documents_survive_a_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let data_dir = dir.path().to_path_buf();

    let token;
    let file_id;
    {
        let harness = Harness::in_dir(Some(data_dir.clone())).await;
        token = harness.register("alice").await;
        put_text(&harness.state, &token, "/private/alice/notes.md", "durable").await;
        let user = harness.state.auth.authenticate(&token).await.unwrap();
        file_id = harness
            .state
            .files
            .get(&user, "/private/alice/notes.md")
            .await
            .unwrap()
            .id
            .0;

        // Snapshot + update log on disk (§16).
        assert!(doc_dir(&data_dir, &file_id).join("snapshot").exists());
        assert!(update_log_entries(&data_dir, &file_id) >= 1);
        let files_before = update_log_entries(&data_dir, &file_id);
        std::mem::forget(harness); // keep the temp dir alive
        assert!(files_before >= 1);
    }

    // Restart: SQLite metadata + snapshot + update log rebuild the YDoc (§17, §18).
    let harness = Harness::restart(data_dir.clone()).await;
    let user = harness.state.auth.authenticate(&token).await.unwrap();
    let snapshot = harness
        .state
        .files
        .read_content(&user, "/private/alice/notes.md")
        .await
        .expect("content after restart");
    assert_eq!(String::from_utf8_lossy(&snapshot.content), "durable");
}

#[tokio::test]
async fn compaction_bounds_the_update_log() {
    let dir = tempfile::tempdir().expect("temp dir");
    let data_dir = dir.path().to_path_buf();
    let config = ServerConfig {
        data_dir: data_dir.clone(),
        documents: meisync_document::DocumentManagerConfig {
            compaction: CompactionPolicy {
                max_updates: 3,
                max_update_bytes: 1 << 30,
                snapshot_bytes: 1 << 30,
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::bootstrap(config).await.expect("bootstrap");
    let token = {
        let user = state.auth.register("alice", "secret123").await.unwrap();
        state
            .files
            .ensure_user_namespace(&user.user.id, &user.user.username)
            .await
            .unwrap();
        user.token
    };

    for index in 0..12 {
        put_text(&state, &token, "/private/alice/log.md", &format!("line {index}\n")).await;
    }

    let user = state.auth.authenticate(&token).await.unwrap();
    let file_id = state
        .files
        .get(&user, "/private/alice/log.md")
        .await
        .unwrap()
        .id
        .0;
    let entries = update_log_entries(&data_dir, &file_id);
    assert!(
        entries <= 3,
        "update log must be compacted, found {entries} entries"
    );

    // The document itself is still correct after compaction.
    let snapshot = state
        .files
        .read_content(&user, "/private/alice/log.md")
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&snapshot.content), "line 11\n");

    // … and after a restart, which replays snapshot + remaining updates.
    drop(state);
    let state = AppState::bootstrap(ServerConfig {
        data_dir: data_dir.clone(),
        documents: meisync_document::DocumentManagerConfig {
            compaction: CompactionPolicy {
                max_updates: 3,
                max_update_bytes: 1 << 30,
                snapshot_bytes: 1 << 30,
            },
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .expect("bootstrap");
    let user = state.auth.authenticate(&token).await.unwrap();
    let snapshot = state
        .files
        .read_content(&user, "/private/alice/log.md")
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&snapshot.content), "line 11\n");
}

#[tokio::test]
async fn full_sync_documents_keep_only_their_state() {
    let dir = tempfile::tempdir().expect("temp dir");
    let data_dir = dir.path().to_path_buf();
    let harness = Harness::in_dir(Some(data_dir.clone())).await;
    let token = harness.register("alice").await;

    // Binary / unknown content types use Full Sync (`docs/mvp.md` §5.2).
    let payload = vec![0u8, 1, 2, 3, 255, 254];
    let user = harness.state.auth.authenticate(&token).await.unwrap();
    harness
        .state
        .files
        .replace_content(
            &user,
            "/private/alice/blob.bin",
            payload.clone(),
            Some("application/octet-stream".into()),
        )
        .await
        .expect("put");
    harness
        .state
        .files
        .replace_content(
            &user,
            "/private/alice/blob.bin",
            vec![9, 9, 9],
            Some("application/octet-stream".into()),
        )
        .await
        .expect("put");

    let meta = harness
        .state
        .files
        .get(&user, "/private/alice/blob.bin")
        .await
        .unwrap();
    assert_eq!(meta.sync_mode, meisync_core::SyncMode::Full);

    let file_id = meta.id.0.clone();
    assert!(
        doc_dir(&data_dir, &file_id).join("snapshot").exists(),
        "full sync documents keep a snapshot"
    );
    assert_eq!(
        update_log_entries(&data_dir, &file_id),
        0,
        "full sync documents must not keep an update log"
    );
    assert_eq!(meta.size, 3);

    // Incremental documents do keep one (§5.1).
    put_text(&harness.state, &token, "/private/alice/text.md", "hello").await;
    let text_meta = harness
        .state
        .files
        .get(&user, "/private/alice/text.md")
        .await
        .unwrap();
    assert_eq!(text_meta.sync_mode, meisync_core::SyncMode::Incremental);
    assert!(update_log_entries(&data_dir, &text_meta.id.0) >= 1);
    let _ = file_id_of(&data_dir, "");
}

#[tokio::test]
async fn concurrent_writes_are_serialised_per_document() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    put_text(&harness.state, &token, "/private/alice/busy.md", "0").await;

    // Every write goes through the single YDoc instance of that file (§24).
    let mut handles = Vec::new();
    for index in 1..=32u32 {
        let state = harness.state.clone();
        let token = token.clone();
        handles.push(tokio::spawn(async move {
            put_text(&state, &token, "/private/alice/busy.md", &index.to_string()).await;
        }));
    }
    for handle in handles {
        handle.await.expect("task");
    }

    let user = harness.state.auth.authenticate(&token).await.unwrap();
    let snapshot = harness
        .state
        .files
        .read_content(&user, "/private/alice/busy.md")
        .await
        .unwrap();
    let content = String::from_utf8_lossy(&snapshot.content).to_string();
    assert!(
        (1..=32).any(|i| i.to_string() == content),
        "unexpected final content: {content}"
    );

    // The version counter advanced exactly once per write (§26).
    let meta = harness
        .state
        .files
        .get(&user, "/private/alice/busy.md")
        .await
        .unwrap();
    // One bump per write: the initial write plus the 32 concurrent ones.
    assert_eq!(meta.version, 33);
}

#[tokio::test]
async fn documents_are_unloaded_from_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = ServerConfig {
        data_dir: dir.path().to_path_buf(),
        documents: meisync_document::DocumentManagerConfig {
            max_cached_documents: 1,
            idle_unload_after: std::time::Duration::from_millis(0),
            janitor_interval: std::time::Duration::from_millis(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::bootstrap(config).await.expect("bootstrap");
    let token = {
        let user = state.auth.register("alice", "secret123").await.unwrap();
        state
            .files
            .ensure_user_namespace(&user.user.id, &user.user.username)
            .await
            .unwrap();
        user.token
    };

    for name in ["a.md", "b.md", "c.md"] {
        put_text(&state, &token, &format!("/private/alice/{name}"), name).await;
    }
    // The cache is bounded (§23).
    assert!(state.documents.cached_len().await <= 1);

    // Idle sweeping removes documents from memory without losing content (§22).
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let evicted = state.documents.sweep().await;
    assert!(evicted > 0 || state.documents.cached_len().await == 0);

    let user = state.auth.authenticate(&token).await.unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        let snapshot = state
            .files
            .read_content(&user, &format!("/private/alice/{name}"))
            .await
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(String::from_utf8_lossy(&snapshot.content), name);
    }
}

#[tokio::test]
async fn status_endpoint_reports_the_document_cache() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    put_text(&harness.state, &token, "/private/alice/a.md", "x").await;

    let response = harness
        .send(harness.request("GET", "/api/status", Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("\"cached_documents\":1"), "{body}");
    assert!(body.contains("max_updates"), "{body}");
}
#[tokio::test]
async fn webdav_writes_keep_an_incremental_document_incremental() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let path = "/private/alice/notes.md";
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &format!("/dav{path}"),
            Some(&token),
            Some("text/markdown"),
            "incremental",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let user = harness.state.auth.authenticate(&token).await.unwrap();
    let meta = harness.state.files.get(&user, path).await.unwrap();
    assert_eq!(meta.sync_mode, SyncMode::Incremental);

    // A checkpoint / compaction (§20) records a snapshot – and must not change the mode.
    harness
        .state
        .documents
        .open(&meta)
        .await
        .expect("open")
        .checkpoint()
        .await
        .expect("checkpoint");
    let ydocs = YDocRepository::new(harness.state.storage.db.clone());
    assert_eq!(
        ydocs.get(&meta.id).await.unwrap().unwrap().sync_mode,
        SyncMode::Incremental,
        "a snapshot must not turn an incremental document into a full sync one"
    );

    // Reload: the mode is adopted from `ydoc_states`, so a wrong value would stop the update
    // log for good.
    harness.state.documents.evict(&meta.id).await;
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &format!("/dav{path}"),
            Some(&token),
            Some("text/markdown"),
            "still incremental",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let stats = harness
        .state
        .storage
        .documents
        .stats(&meta.id)
        .await
        .expect("stats");
    assert!(
        stats.update_count >= 1,
        "the update log must keep growing after a reload"
    );
    assert_eq!(
        ydocs.get(&meta.id).await.unwrap().unwrap().sync_mode,
        SyncMode::Incremental
    );
}

#[tokio::test]
async fn the_document_version_survives_a_reload() {
    let harness = Harness::spawn().await;
    let token = harness.register("alice").await;
    let path = "/private/alice/version.md";
    for index in 0..3 {
        let response = harness
            .send(harness.request_with_body(
                "PUT",
                &format!("/dav{path}"),
                Some(&token),
                Some("text/markdown"),
                format!("v{index}"),
            ))
            .await;
        assert!(response.status().is_success());
    }

    let user = harness.state.auth.authenticate(&token).await.unwrap();
    let meta = harness.state.files.get(&user, path).await.unwrap();
    assert_eq!(meta.version, 3);

    // Unload + reload must not reset the monotonic revision (§26).
    harness.state.documents.evict(&meta.id).await;
    let response = harness
        .send(harness.request("GET", &format!("/dav{path}"), Some(&token)))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let version: i64 = response
        .headers()
        .get("x-meisync-version")
        .expect("version header")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(version, 3, "the revision must survive an unload");

    // … and the next write continues at N+1 instead of starting over.
    let response = harness
        .send(harness.request_with_body(
            "PUT",
            &format!("/dav{path}"),
            Some(&token),
            Some("text/markdown"),
            "v3",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let meta = harness.state.files.get(&user, path).await.unwrap();
    assert_eq!(meta.version, 4);
    let response = harness
        .send(harness.request("GET", &format!("/api/document{path}"), Some(&token)))
        .await;
    let body = body_string(response).await;
    assert!(body.contains("\"version\":4"), "{body}");
}
