use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use meisync_core::{
    guess_sync_mode, resolve_content_type, AclEntry, AclProvider, FileDocument, FileId, FileKind,
    FileMetadata, MeiSyncError, PermissionService, PermissionSet, Preconditions, Result, User,
    Visibility, PRIVATE_ROOT, PUBLIC_ROOT,
};
use meisync_document::{DocumentHandle, DocumentManager, DocumentSnapshot};
use meisync_persistence::{AclRepository, BucketRepository, FileRepository};

use crate::events::{ChangeHub, ChangeKind, FileChange};

/// The virtual root of the namespace (`/`). It is a real row so that every other row has a
/// parent, which is what makes `UNIQUE(parent_id, name)` work.
pub const ROOT_ID: &str = "root";
/// Owner id used for the bootstrap collections.
pub const SYSTEM_OWNER: &str = "system";

pub fn root_id() -> FileId {
    FileId::new(ROOT_ID)
}

/// How deep a listing should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Zero,
    One,
    Infinite,
}

impl Depth {
    pub fn parse(value: &str) -> Depth {
        match value.trim().to_ascii_lowercase().as_str() {
            "0" => Depth::Zero,
            "infinity" => Depth::Infinite,
            _ => Depth::One,
        }
    }

    pub fn as_header(&self) -> &'static str {
        match self {
            Depth::Zero => "0",
            Depth::One => "1",
            Depth::Infinite => "infinity",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateFileSpec {
    pub parent_path: String,
    pub name: String,
    pub kind: FileKind,
    pub content_type: Option<String>,
    pub visibility: Option<Visibility>,
    pub initial_content: Option<Vec<u8>>,
}

/// Arguments of a recursive listing (`ListObjectsV2`).
#[derive(Debug, Clone)]
pub struct RecursiveListSpec<'a> {
    /// The bucket collection the listing starts at.
    pub root: &'a FileMetadata,
    /// Namespace path of `root`; returned keys are relative to it.
    pub base_path: &'a str,
    pub prefix: &'a str,
    pub delimiter: Option<&'a str>,
    /// Continuation: only keys strictly after this one are returned.
    pub start_after: Option<&'a str>,
    pub max_keys: usize,
}

/// Result of a recursive listing.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub objects: Vec<FileMetadata>,
    pub prefixes: Vec<String>,
    /// `true` when the listing was cut at `max_keys` (→ `IsTruncated`).
    pub truncated: bool,
    /// Continuation token of the next page (→ `NextContinuationToken`): the last element the
    /// page emitted from the merged object / `CommonPrefixes` sequence, either an object key or
    /// a prefix root. Only ever set together with `truncated`.
    pub next_continuation_token: Option<String>,
}

/// The unified file API used by *all* protocols (`docs/mvp.md` §33).
///
/// WebDAV, S3 and YSync never touch storage directly – they go through this service, which in
/// turn goes through [`PermissionService`] and [`DocumentManager`].
#[async_trait]
pub trait FileService: Send + Sync {
    async fn create(&self, actor: &User, spec: CreateFileSpec) -> Result<FileMetadata>;

    async fn get(&self, actor: &User, path: &str) -> Result<FileMetadata>;

    async fn get_by_id(&self, actor: &User, id: &FileId) -> Result<FileMetadata>;

    async fn list(&self, actor: &User, path: &str, depth: Depth) -> Result<Vec<FileMetadata>>;

    /// Delete a path without any conditional header.
    async fn delete(&self, actor: &User, path: &str) -> Result<()> {
        self.delete_checked(actor, path, Preconditions::default())
            .await
    }

    /// Same as [`FileService::delete`], but the `If-Match` / `If-None-Match` preconditions are
    /// evaluated against the live document right before it is closed (§25). The preconditions
    /// apply to the target path only; a recursive child deletion stays unconditional.
    async fn delete_checked(
        &self,
        actor: &User,
        path: &str,
        preconditions: Preconditions,
    ) -> Result<()>;

    async fn move_file(
        &self,
        actor: &User,
        from: &str,
        to_parent: &str,
        new_name: Option<&str>,
        overwrite: bool,
    ) -> Result<FileMetadata>;

    async fn read_content(&self, actor: &User, path: &str) -> Result<DocumentSnapshot>;

    /// The full `FileDocument` aggregate of §3.1: metadata *and* the live YDoc.
    async fn document(&self, actor: &User, path: &str) -> Result<FileDocument>;

    async fn replace_content(
        &self,
        actor: &User,
        path: &str,
        body: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<DocumentSnapshot> {
        self.replace_content_checked(actor, path, body, content_type, Preconditions::default())
            .await
    }

    /// Same as [`FileService::replace_content`], but the `If-Match` / `If-None-Match`
    /// preconditions are evaluated inside the document lock (§25).
    async fn replace_content_checked(
        &self,
        actor: &User,
        path: &str,
        body: Vec<u8>,
        content_type: Option<String>,
        preconditions: Preconditions,
    ) -> Result<DocumentSnapshot>;

    /// YSync entry point: apply a CRDT update received from a client.
    async fn apply_update(
        &self,
        actor: &User,
        id: &FileId,
        update: Vec<u8>,
        except: Option<meisync_document::ConnId>,
    ) -> Result<DocumentSnapshot>;

    /// Open the document behind a path (YSync room join).
    async fn open_document(
        &self,
        actor: &User,
        path: &str,
    ) -> Result<(FileMetadata, DocumentHandle)>;

    /// Absolute namespace path of a document, e.g. `/private/alice/todo.md`.
    async fn path_of(&self, id: &FileId) -> Result<String>;

    async fn set_visibility(&self, actor: &User, id: &FileId, visibility: Visibility)
        -> Result<()>;

    async fn acl_list(&self, actor: &User, id: &FileId) -> Result<Vec<AclEntry>>;

    async fn acl_grant(
        &self,
        actor: &User,
        id: &FileId,
        user: &meisync_core::UserId,
        permissions: PermissionSet,
    ) -> Result<()>;

    /// Recursive listing used by S3 `ListObjectsV2`.
    async fn list_recursive(&self, actor: &User, spec: &RecursiveListSpec<'_>) -> Result<Listing>;

    async fn bucket_collection(&self, bucket: &str) -> Result<FileMetadata>;

    async fn buckets(&self) -> Result<Vec<(String, FileId, Option<String>)>>;

    /// Register a top level collection as an S3 bucket.
    async fn register_bucket(
        &self,
        name: &str,
        collection_id: &FileId,
        owner: &meisync_core::UserId,
    ) -> Result<()>;
}

/// Configuration that influences permission defaults.
#[derive(Debug, Clone, Default)]
pub struct FileServiceConfig {
    /// When enabled, every authenticated user may write public files (see `docs/mvp.md` §12).
    pub public_default_write: bool,
}

pub struct DefaultFileService {
    files: Arc<FileRepository>,
    acl: Arc<AclRepository>,
    buckets: Arc<BucketRepository>,
    documents: Arc<DocumentManager>,
    permissions: Arc<dyn PermissionService>,
    config: FileServiceConfig,
    changes: Arc<ChangeHub>,
}

impl DefaultFileService {
    pub fn new(
        files: Arc<FileRepository>,
        acl: Arc<AclRepository>,
        buckets: Arc<BucketRepository>,
        documents: Arc<DocumentManager>,
        permissions: Arc<dyn PermissionService>,
        config: FileServiceConfig,
        changes: Arc<ChangeHub>,
    ) -> Arc<Self> {
        Arc::new(Self {
            files,
            acl,
            buckets,
            documents,
            permissions,
            config,
            changes,
        })
    }

    pub fn changes(&self) -> &Arc<ChangeHub> {
        &self.changes
    }

    fn announce(
        &self,
        actor_id: &str,
        kind: ChangeKind,
        meta: &FileMetadata,
        path: String,
        old_path: Option<String>,
    ) {
        self.changes.publish(FileChange {
            seq: 0,
            kind,
            file_id: meta.id.0.clone(),
            path,
            old_path,
            etag: meta.etag.clone(),
            size: meta.size,
            collection: meta.is_collection(),
            updated_at: meta.updated_at,
            actor_id: actor_id.to_string(),
        });
    }

    // -----------------------------------------------------------------------------------------
    // Namespace bootstrap
    // -----------------------------------------------------------------------------------------

    /// Create the virtual root and the `private` / `public` top level collections.
    pub async fn bootstrap(self: &Arc<Self>) -> Result<()> {
        let now = Utc::now();
        let root = root_id();
        if self.files.find_by_id(&root).await?.is_none() {
            let meta = FileMetadata {
                id: root.clone(),
                parent_id: None,
                name: String::new(),
                owner_id: SYSTEM_OWNER.into(),
                visibility: Visibility::Public,
                content_type: None,
                kind: FileKind::Collection,
                created_at: now,
                updated_at: now,
                size: 0,
                etag: None,
                version: 0,
                sync_mode: meisync_core::SyncMode::Incremental,
            };
            self.files.insert(meta).await?;
        }

        // `/private` is only a *container*: it must be traversable and listable so that each
        // user's private namespace can be found, while every child keeps its own `private`
        // visibility and is filtered out of listings for everybody else (§4, §12).
        let private = self
            .ensure_collection(&root, PRIVATE_ROOT, SYSTEM_OWNER.into(), Visibility::Public)
            .await?;
        let public = self
            .ensure_collection(&root, PUBLIC_ROOT, SYSTEM_OWNER.into(), Visibility::Public)
            .await?;

        // `/public` is exposed as a bucket; per user buckets are created on registration.
        self.buckets.upsert(PUBLIC_ROOT, &public.id, None).await?;
        let _ = private;
        Ok(())
    }

    async fn ensure_collection(
        &self,
        parent: &FileId,
        name: &str,
        owner: meisync_core::UserId,
        visibility: Visibility,
    ) -> Result<FileMetadata> {
        if let Some(existing) = self.files.find_child(parent, name).await? {
            return Ok(existing);
        }
        let now = Utc::now();
        self.files
            .insert(FileMetadata {
                id: FileId::generate(),
                parent_id: Some(parent.clone()),
                name: name.to_string(),
                owner_id: owner,
                visibility,
                content_type: None,
                kind: FileKind::Collection,
                created_at: now,
                updated_at: now,
                size: 0,
                etag: None,
                version: 0,
                sync_mode: meisync_core::SyncMode::Incremental,
            })
            .await
    }

    /// Per-user namespace: `/private/<username>` plus a bucket named after the user (§4, §30).
    pub async fn ensure_user_namespace(
        self: &Arc<Self>,
        user_id: &meisync_core::UserId,
        username: &str,
    ) -> Result<FileMetadata> {
        let root = root_id();
        // `/private` is only a *container*: it must be traversable and listable so that each
        // user's private namespace can be found, while every child keeps its own `private`
        // visibility and is filtered out of listings for everybody else (§4, §12).
        let private = self
            .ensure_collection(&root, PRIVATE_ROOT, SYSTEM_OWNER.into(), Visibility::Public)
            .await?;
        // A collection that squatting already created under this name must never be adopted as
        // somebody's home: `/private/<username>` has to be a private collection of that user.
        let home = match self.files.find_child(&private.id, username).await? {
            Some(existing) => {
                if existing.kind != FileKind::Collection || existing.owner_id != *user_id {
                    return Err(MeiSyncError::Conflict(format!(
                        "/{PRIVATE_ROOT}/{username} already exists and does not belong to this account"
                    )));
                }
                if existing.visibility != Visibility::Private {
                    // An owner who made their own home public gets it forced back instead of
                    // being locked out: `/private/<username>` is never a shared collection.
                    self.files
                        .set_visibility(&existing.id, Visibility::Private)
                        .await?;
                    self.files
                        .find_by_id(&existing.id)
                        .await?
                        .unwrap_or(existing)
                } else {
                    existing
                }
            }
            None => {
                self.ensure_collection(&private.id, username, user_id.clone(), Visibility::Private)
                    .await?
            }
        };
        self.buckets
            .upsert(username, &home.id, Some(user_id))
            .await?;
        Ok(home)
    }

    // -----------------------------------------------------------------------------------------
    // Path resolution
    // -----------------------------------------------------------------------------------------

    /// Split an absolute namespace path into validated segments.
    pub fn split_path(path: &str) -> Vec<String> {
        path.split('/')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != ".")
            .map(|s| s.to_string())
            .collect()
    }

    pub fn join_path(parent: &str, name: &str) -> String {
        if parent.ends_with('/') {
            format!("{parent}{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    pub fn normalize_path(path: &str) -> String {
        let segments = Self::split_path(path);
        if segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segments.join("/"))
        }
    }

    /// Resolve a path for `actor`.
    ///
    /// Intermediate nodes are traversed without a permission check: they are only containers,
    /// confidentiality is enforced on the node that is actually returned (and on every child of a
    /// listing). Checking intermediate nodes would hide `/private/<other user>` while still
    /// allowing `/private/<own user>`, so the visibility of a *leaf* is what decides.
    pub async fn resolve(&self, _actor: &User, path: &str) -> Result<FileMetadata> {
        self.resolve_path(path).await
    }

    /// Path resolution without a permission check. Callers that are not protocol frontends
    /// (the admin console, sync root provisioning) must enforce their own authorisation.
    pub async fn resolve_path(&self, path: &str) -> Result<FileMetadata> {
        let segments = Self::split_path(path);
        let mut current = self
            .files
            .find_by_id(&root_id())
            .await?
            .ok_or_else(|| MeiSyncError::internal("namespace root is missing"))?;
        let mut walked = String::new();
        for segment in segments {
            walked.push('/');
            walked.push_str(&segment);
            let child = self.files.find_child(&current.id, &segment).await?;
            current = match child {
                Some(child) => child,
                None => return Err(MeiSyncError::NotFound(walked)),
            };
        }
        Ok(current)
    }

    // -----------------------------------------------------------------------------------------
    // Admin helpers
    // -----------------------------------------------------------------------------------------

    /// Node plus descendants, without filtering by permission. Used by the admin file browser.
    pub async fn admin_list(&self, path: &str, depth: Depth) -> Result<Vec<FileMetadata>> {
        let meta = self.resolve_path(path).await?;
        let mut out = vec![meta.clone()];
        if depth == Depth::Zero {
            return Ok(out);
        }
        let mut queue = vec![meta];
        while let Some(current) = queue.pop() {
            for child in self.files.list_children(&current.id).await? {
                if child.is_collection() && depth == Depth::Infinite {
                    queue.push(child.clone());
                }
                out.push(child);
            }
        }
        Ok(out)
    }

    pub async fn admin_read_content(&self, path: &str) -> Result<(FileMetadata, DocumentSnapshot)> {
        let meta = self.resolve_path(path).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict("path is a collection".into()));
        }
        let handle = self.documents.open(&meta).await?;
        let snapshot = handle.read().await;
        Ok((meta, snapshot))
    }

    /// Recursively delete a namespace entry, bypassing the owner check. Every YDoc is closed
    /// before the rows disappear, so the on-disk directories are removed through the normal
    /// lifecycle instead of being leaked.
    pub async fn admin_delete_path(&self, path: &str) -> Result<()> {
        let normalized = Self::normalize_path(path);
        if normalized == "/" || normalized == "/private" || normalized == "/public" {
            return Err(MeiSyncError::Forbidden(
                "the root, /private and /public containers cannot be deleted".into(),
            ));
        }
        let meta = self.resolve_path(path).await?;
        if meta.id.as_str() == ROOT_ID {
            return Err(MeiSyncError::Forbidden("cannot delete the root".into()));
        }
        let mut entries: Vec<(FileMetadata, String)> = Vec::new();
        for descendant in self.files.list_descendants(&meta.id).await? {
            let descendant_path = self.path_of(&descendant.id).await?;
            entries.push((descendant, descendant_path));
        }
        let meta_path = self.path_of(&meta.id).await?;
        entries.push((meta.clone(), meta_path));

        for (entry, _) in &entries {
            self.documents.close(&entry.id).await?;
        }
        self.files.delete(&meta.id).await?;
        for (entry, entry_path) in entries {
            if entry.is_collection() {
                self.buckets.delete_for_collection(&entry.id).await?;
            }
            self.announce("admin", ChangeKind::Deleted, &entry, entry_path, None);
        }
        Ok(())
    }

    /// Purge `/private/<username>` when an administrator removes an account.
    pub async fn admin_delete_namespace(&self, username: &str) -> Result<()> {
        let path = format!("/{PRIVATE_ROOT}/{username}");
        match self.admin_delete_path(&path).await {
            Ok(()) => Ok(()),
            Err(MeiSyncError::NotFound(_)) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Provision a collection path (including every missing parent). Used by sync roots.
    pub async fn ensure_collection_path(&self, actor: &User, path: &str) -> Result<FileMetadata> {
        self.ensure_parent_chain(actor, path).await
    }

    /// Split a *child* path into `(parent_path, name)`.
    pub fn split_parent(path: &str) -> Result<(String, String)> {
        let normalized = Self::normalize_path(path);
        let segments = Self::split_path(&normalized);
        let (name, rest) = match segments.split_last() {
            Some((name, rest)) => (name.clone(), rest),
            None => {
                return Err(MeiSyncError::bad_request(
                    "cannot operate on the namespace root",
                ))
            }
        };
        FileMetadata::validate_name(&name)?;
        let parent_path = if rest.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rest.join("/"))
        };
        Ok((parent_path, name))
    }

    /// Resolve the parent *directory* of `path` (used by PUT / MKCOL of a missing child).
    ///
    /// `path` is the full child path, e.g. `/private/alice/notes/todo.md` resolves
    /// `/private/alice/notes`.
    pub async fn resolve_parent(&self, actor: &User, path: &str) -> Result<FileMetadata> {
        let (parent_path, _name) = Self::split_parent(path)?;
        self.resolve_existing_parent(actor, &parent_path).await
    }

    /// Resolve a path that is expected to exist and to be a collection.
    pub async fn resolve_existing_parent(
        &self,
        actor: &User,
        parent_path: &str,
    ) -> Result<FileMetadata> {
        let parent = self.resolve(actor, parent_path).await?;
        if !parent.is_collection() {
            return Err(MeiSyncError::Conflict(format!(
                "{parent_path} is not a collection"
            )));
        }
        Ok(parent)
    }

    /// Create every missing collection on the way to `parent_path`.
    ///
    /// WebDAV clients and S3 keys both expect `PUT a/b/c.txt` to work even when `a/b` does not
    /// exist yet (S3 has no directories at all). Missing collections inherit the visibility of
    /// their closest existing ancestor and are owned by the caller.
    pub async fn ensure_parent_chain(
        &self,
        actor: &User,
        parent_path: &str,
    ) -> Result<FileMetadata> {
        let mut current = self
            .files
            .find_by_id(&root_id())
            .await?
            .ok_or_else(|| MeiSyncError::internal("namespace root is missing"))?;
        let mut current_path = String::new();
        let mut inherited: Option<Visibility> = Some(Visibility::Private);

        for segment in Self::split_path(parent_path) {
            current_path.push('/');
            current_path.push_str(&segment);
            if let Some(child) = self.files.find_child(&current.id, &segment).await? {
                inherited = Some(child.visibility);
                current = child;
                continue;
            }
            // Top level collections (buckets) and the shared `/public` drop box may be created
            // in / extended by any authenticated user.
            let allowed = current.id.as_str() == ROOT_ID
                || (current.visibility == Visibility::Public
                    && current.owner_id.as_str() == SYSTEM_OWNER)
                || self
                    .effective_permissions(actor, &current)
                    .await?
                    .has(PermissionSet::WRITE);
            if !allowed {
                let path = current_path.clone();
                if self.permissions.can_read_metadata(actor, &current).await? {
                    return Err(MeiSyncError::Forbidden(format!("cannot create {path}")));
                }
                return Err(MeiSyncError::NotFound(path));
            }
            current = self
                .create_in_parent(
                    actor,
                    &current,
                    CreateFileSpec {
                        parent_path: current_path.clone(),
                        name: segment.clone(),
                        kind: FileKind::Collection,
                        content_type: None,
                        visibility: inherited,
                        initial_content: None,
                    },
                )
                .await?;
        }
        Ok(current)
    }

    async fn child_name_of(&self, meta: &FileMetadata) -> Result<String> {
        self.path_of(&meta.id).await
    }

    // -----------------------------------------------------------------------------------------
    // Permission helpers
    // -----------------------------------------------------------------------------------------

    /// Insert a new child into an already resolved collection.
    async fn create_in_parent(
        &self,
        actor: &User,
        parent: &FileMetadata,
        spec: CreateFileSpec,
    ) -> Result<FileMetadata> {
        FileMetadata::validate_name(&spec.name)?;
        if !parent.is_collection() {
            return Err(MeiSyncError::Conflict(format!(
                "{} is not a collection",
                self.path_of(&parent.id).await.unwrap_or_default()
            )));
        }
        // Top level collections are buckets: any authenticated user may create one, which is
        // how `CreateBucket` and the per user namespaces work (§4, §30).
        let creating_bucket = parent.id.as_str() == ROOT_ID;
        if creating_bucket && spec.kind == FileKind::File {
            // The namespace root only holds collections, because a collection is what the S3
            // API addresses as a bucket. A file directly under `/` could never be reached
            // through S3 (`GET /s3/<name>` is a bucket listing), so it is refused instead of
            // silently becoming unreachable through a second protocol.
            return Err(MeiSyncError::Conflict(format!(
                "cannot create a file at the namespace root: put it into a collection \
                 (for example /private/{}/...), the namespace root only holds buckets",
                actor.username
            )));
        }
        if !creating_bucket {
            // `/private` is a system owned container whose children are the per-user homes: the
            // server provisions them at registration and a client request must never occupy
            // `/private/<name>` before its owner registers (§4).
            if is_private_root(parent) {
                return Err(MeiSyncError::Forbidden(format!(
                    "/{PRIVATE_ROOT}/{} is provisioned by the server at registration and cannot \
                     be created by clients",
                    spec.name
                )));
            }
            let perms = self.effective_permissions(actor, parent).await?;
            // The system owned `/public` collection is the shared drop box: every authenticated
            // user may publish there (§12).
            let shared_dropbox =
                parent.visibility == Visibility::Public && parent.owner_id.as_str() == SYSTEM_OWNER;
            if !perms.has(PermissionSet::WRITE) && !shared_dropbox {
                // A collection the caller cannot even read must not be revealed as "exists but
                // forbidden" (README: private files of other users answer 404, never 403).
                let path = self.path_of(&parent.id).await.unwrap_or_default();
                if perms.has(PermissionSet::READ) {
                    return Err(MeiSyncError::Forbidden(format!(
                        "cannot create children in {path}"
                    )));
                }
                return Err(MeiSyncError::NotFound(path));
            }
        }
        if self
            .files
            .find_child(&parent.id, &spec.name)
            .await?
            .is_some()
        {
            return Err(MeiSyncError::AlreadyExists(format!(
                "{} already exists",
                Self::join_path(&self.path_of(&parent.id).await?, &spec.name)
            )));
        }

        let content_type = spec.content_type.clone().or_else(|| {
            if spec.kind == FileKind::Collection {
                None
            } else {
                Some(meisync_core::guess_content_type(&spec.name))
            }
        });
        // A new child inherits the visibility of its collection: files created inside
        // `/public/...` are public, files inside a private namespace stay private (§4).
        let visibility = spec
            .visibility
            .or_else(|| {
                if parent.id.as_str() == ROOT_ID {
                    None
                } else {
                    Some(parent.visibility)
                }
            })
            .unwrap_or(Visibility::Private);
        let sync_mode = guess_sync_mode(content_type.as_deref());
        let now = Utc::now();
        let id = FileId::generate();
        let meta = FileMetadata {
            id: id.clone(),
            parent_id: Some(parent.id.clone()),
            name: spec.name.clone(),
            owner_id: actor.id.clone(),
            visibility,
            content_type,
            kind: spec.kind,
            created_at: now,
            updated_at: now,
            size: 0,
            etag: None,
            version: 0,
            sync_mode,
        };
        let meta = self.files.insert(meta.clone()).await?;

        if spec.kind == FileKind::File {
            match spec.initial_content.clone() {
                Some(content) => {
                    let handle = self.documents.open(&meta).await?;
                    handle
                        .replace_content(&actor.id, content, meta.content_type.clone())
                        .await?;
                }
                None => {
                    // Make sure a document exists on disk for a brand new file.
                    let handle = self.documents.open(&meta).await?;
                    handle.checkpoint().await?;
                }
            }
        }
        let refreshed = self.files.find_by_id(&id).await?.unwrap_or(meta);
        let path = self.path_of(&id).await?;
        self.announce(&actor.id.0, ChangeKind::Created, &refreshed, path, None);
        Ok(refreshed)
    }

    async fn require_read(&self, actor: &User, meta: &FileMetadata) -> Result<()> {
        if meta.is_collection() && meta.id.as_str() == ROOT_ID {
            return Ok(());
        }
        if self.permissions.can_read_metadata(actor, meta).await? {
            Ok(())
        } else {
            Err(MeiSyncError::NotFound(
                self.path_of(&meta.id).await.unwrap_or_default(),
            ))
        }
    }

    async fn require_write(&self, actor: &User, meta: &FileMetadata) -> Result<()> {
        let perms = self.effective_permissions(actor, meta).await?;
        if perms.has(PermissionSet::WRITE) {
            return Ok(());
        }
        let path = self.path_of(&meta.id).await.unwrap_or_default();
        // A node the caller cannot even read must not be revealed as "exists but forbidden"
        // (README: private files of other users answer 404, never 403).
        if !perms.has(PermissionSet::READ) {
            return Err(MeiSyncError::NotFound(path));
        }
        Err(MeiSyncError::Forbidden(format!(
            "write access denied: {path}"
        )))
    }

    async fn require_sync(&self, actor: &User, meta: &FileMetadata) -> Result<()> {
        if self.permissions.can_sync_metadata(actor, meta).await? {
            Ok(())
        } else {
            Err(MeiSyncError::Forbidden(format!(
                "sync access denied: {}",
                self.path_of(&meta.id).await.unwrap_or_default()
            )))
        }
    }

    async fn require_delete(&self, actor: &User, meta: &FileMetadata) -> Result<()> {
        // Same permission view as every other operation, including the `public_default_write`
        // server option – otherwise DELETE would be stricter than WRITE on public files.
        let perms = self.effective_permissions(actor, meta).await?;
        if perms.has(PermissionSet::DELETE) {
            return Ok(());
        }
        let path = self.path_of(&meta.id).await.unwrap_or_default();
        // Invisible nodes answer 404, visible ones 403 (README: private files of other users
        // answer 404, never 403).
        if !perms.has(PermissionSet::READ) {
            return Err(MeiSyncError::NotFound(path));
        }
        Err(MeiSyncError::Forbidden(format!(
            "delete access denied: {path}"
        )))
    }

    /// Effective permission set, honouring the `public_default_write` server option.
    pub async fn effective_permissions(
        &self,
        actor: &User,
        meta: &FileMetadata,
    ) -> Result<PermissionSet> {
        let mut perms = self.permissions.permissions_for(actor, meta).await?;
        if self.config.public_default_write
            && meta.visibility == Visibility::Public
            && perms.has(PermissionSet::READ)
        {
            perms.insert(PermissionSet::WRITE | PermissionSet::DELETE);
        }
        Ok(perms)
    }
}

#[async_trait]
impl FileService for DefaultFileService {
    async fn create(&self, actor: &User, spec: CreateFileSpec) -> Result<FileMetadata> {
        let parent = self
            .resolve_existing_parent(actor, &spec.parent_path)
            .await?;
        self.create_in_parent(actor, &parent, spec).await
    }

    async fn get(&self, actor: &User, path: &str) -> Result<FileMetadata> {
        let meta = self.resolve(actor, path).await?;
        self.require_read(actor, &meta).await?;
        Ok(meta)
    }

    async fn get_by_id(&self, actor: &User, id: &FileId) -> Result<FileMetadata> {
        let meta = self
            .files
            .find_by_id(id)
            .await?
            .ok_or_else(|| MeiSyncError::NotFound(id.0.clone()))?;
        self.require_read(actor, &meta).await?;
        Ok(meta)
    }

    async fn list(&self, actor: &User, path: &str, depth: Depth) -> Result<Vec<FileMetadata>> {
        let meta = self.resolve(actor, path).await?;
        self.require_read(actor, &meta).await?;
        let mut out = Vec::new();
        let children = self.files.list_children(&meta.id).await?;
        for child in children {
            if self.permissions.can_read_metadata(actor, &child).await? {
                let is_collection = child.is_collection();
                out.push(child.clone());
                if is_collection && depth == Depth::Infinite {
                    let child_path = self.child_name_of(&child).await?;
                    let nested = self.list(actor, &child_path, depth).await?;
                    out.extend(nested);
                }
            }
        }
        Ok(out)
    }

    async fn delete_checked(
        &self,
        actor: &User,
        path: &str,
        preconditions: Preconditions,
    ) -> Result<()> {
        let meta = self.resolve(actor, path).await?;
        if meta.id.as_str() == ROOT_ID {
            return Err(MeiSyncError::Forbidden("cannot delete the root".into()));
        }
        self.require_delete(actor, &meta).await?;
        let deleted_path = self
            .path_of(&meta.id)
            .await
            .unwrap_or_else(|_| meta.name.clone());

        // A collection carries no document ETag, so its preconditions are settled before anything
        // below it is touched: a rejected DELETE must not leave a half-deleted tree behind.
        if meta.is_collection() {
            preconditions.check_write(None)?;
        }

        // Delete children first: close their rooms and drop their YDoc artefacts (§28). The
        // conditional headers of the request belong to the target path, so the recursion keeps
        // the unconditional semantics of [`FileService::delete`].
        let children = self.files.list_children(&meta.id).await?;
        for child in children {
            let child_path = self.child_name_of(&child).await?;
            self.delete(actor, &child_path).await?;
        }

        let descendants = self.files.list_descendants(&meta.id).await?;
        for descendant in &descendants {
            self.documents.close(&descendant.id).await?;
        }
        // §25: `If-Match` / `If-None-Match` are evaluated against the live document, immediately
        // before it is closed. Reading the ETag from a metadata row first would let a YSync update
        // or a WebDAV PUT land in the window between the check and the delete and be destroyed,
        // even though the client sent the ETag it had observed.
        //
        // An unconditional DELETE carries no conditional headers, so there is nothing to evaluate:
        // it must not pay for a document load, and it must not start failing on a persisted state
        // the caller never asked to read.
        if !meta.is_collection() && !preconditions.is_empty() {
            let etag = self.documents.open(&meta).await?.etag().await;
            preconditions.check_write(Some(&etag))?;
        }
        self.documents.close(&meta.id).await?;
        self.files.delete(&meta.id).await?;
        self.announce(&actor.id.0, ChangeKind::Deleted, &meta, deleted_path, None);
        Ok(())
    }

    async fn move_file(
        &self,
        actor: &User,
        from: &str,
        to_parent: &str,
        new_name: Option<&str>,
        overwrite: bool,
    ) -> Result<FileMetadata> {
        let mut meta = self.resolve(actor, from).await?;
        if meta.id.as_str() == ROOT_ID {
            return Err(MeiSyncError::Forbidden("cannot move the root".into()));
        }
        self.require_write(actor, &meta).await?;
        let old_path = self
            .path_of(&meta.id)
            .await
            .unwrap_or_else(|_| from.to_string());

        let dest_parent = self.resolve(actor, to_parent).await?;
        // `/private/<name>` is provisioned by the server at registration and must not be
        // occupied by a MOVE either, otherwise `--public-default-write` (which grants WRITE on
        // the system owned `/private` container) would re-open the squatting path (§4).
        if is_private_root(&dest_parent) {
            let name = new_name.unwrap_or(meta.name.as_str());
            return Err(MeiSyncError::Forbidden(format!(
                "/{PRIVATE_ROOT}/{name} is provisioned by the server at registration and cannot \
                 be created by clients"
            )));
        }
        let perms = self.effective_permissions(actor, &dest_parent).await?;
        if !perms.has(PermissionSet::WRITE) {
            let path = self.path_of(&dest_parent.id).await.unwrap_or_default();
            // A destination the caller cannot read stays hidden, like in `create_in_parent`.
            if !perms.has(PermissionSet::READ) {
                return Err(MeiSyncError::NotFound(path));
            }
            return Err(MeiSyncError::Forbidden(
                "cannot move a file into that collection".into(),
            ));
        }

        let name = new_name
            .map(|n| n.to_string())
            .unwrap_or_else(|| meta.name.clone());
        FileMetadata::validate_name(&name)?;

        let same_parent = meta
            .parent_id
            .as_ref()
            .map(|id| id.as_str() == dest_parent.id.as_str())
            .unwrap_or(false);
        // Moving a collection into its own subtree is illegal. This runs before the overwrite
        // deletes below, so a rejected MOVE never mutates the destination (§29).
        if !same_parent && meta.is_collection() {
            let descendants = self.files.list_descendants(&meta.id).await?;
            if descendants.iter().any(|d| d.id == dest_parent.id) {
                return Err(MeiSyncError::bad_request(
                    "cannot move a collection into itself",
                ));
            }
        }
        if same_parent {
            // Pure rename inside the same collection.
            if let Some(existing) = self.files.find_child(&dest_parent.id, &name).await? {
                if existing.id != meta.id {
                    if !overwrite {
                        return Err(MeiSyncError::AlreadyExists(format!(
                            "{name} already exists"
                        )));
                    }
                    self.delete(actor, &Self::join_path(to_parent, &name))
                        .await?;
                }
            }
        } else {
            if let Some(existing) = self.files.find_child(&dest_parent.id, &name).await? {
                if existing.id != meta.id {
                    if !overwrite {
                        return Err(MeiSyncError::AlreadyExists(format!(
                            "{name} already exists"
                        )));
                    }
                    self.delete(actor, &Self::join_path(to_parent, &name))
                        .await?;
                }
            }
            meta.parent_id = Some(dest_parent.id.clone());
        }

        meta.name = name;
        meta.updated_at = Utc::now();
        self.files.update_meta(&meta).await?;

        let new_path = self.path_of(&meta.id).await?;
        self.documents
            .notify_moved(&meta.id, new_path.clone())
            .await;
        let refreshed = self.files.find_by_id(&meta.id).await?.unwrap_or(meta);
        self.announce(&actor.id.0, ChangeKind::Moved, &refreshed, new_path, Some(old_path));
        Ok(refreshed)
    }

    async fn read_content(&self, actor: &User, path: &str) -> Result<DocumentSnapshot> {
        let meta = self.resolve(actor, path).await?;
        self.require_read(actor, &meta).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict("path is a collection".into()));
        }
        let handle = self.documents.open(&meta).await?;
        Ok(handle.read().await)
    }

    async fn document(&self, actor: &User, path: &str) -> Result<FileDocument> {
        let mut meta = self.resolve(actor, path).await?;
        self.require_read(actor, &meta).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict("path is a collection".into()));
        }
        let handle = self.documents.open(&meta).await?;
        let snapshot = handle.read().await;
        // The aggregate exposes the *current* YDoc state, so the cached metadata is refreshed.
        meta.size = snapshot.size;
        meta.etag = Some(snapshot.etag);
        meta.version = snapshot.version;
        meta.updated_at = Utc::now();
        Ok(FileDocument::new(meta, handle.raw_doc().await))
    }

    async fn replace_content_checked(
        &self,
        actor: &User,
        path: &str,
        body: Vec<u8>,
        content_type: Option<String>,
        preconditions: Preconditions,
    ) -> Result<DocumentSnapshot> {
        // One resolution point for every front end (WebDAV PUT, S3 PutObject, JSON PUT, COPY):
        // the declared type decides, unless it is a generic "unknown bytes" type, in which case
        // the file name does. Without this an `application/octet-stream` upload of `notes.md`
        // (curl, Apifox, most S3 SDKs) would be stored base64 encoded and lose incremental sync
        // for good (§5.2). The resolved type is also what gets persisted and reported back.
        let name = path.rsplit('/').next().unwrap_or(path);
        let content_type = Some(resolve_content_type(content_type.as_deref(), name));

        let (meta, existed) = match self.resolve(actor, path).await {
            Ok(meta) => (meta, true),
            Err(MeiSyncError::NotFound(_)) => {
                // A conditional write must not create a missing resource (§25).
                preconditions.check_write(None)?;
                // WebDAV PUT / S3 PutObject create the file (and any missing parent
                // collection) when it does not exist yet: S3 keys have no directories, so
                // `s3://bucket/a/b/c.txt` must just work.
                let (parent_path, name) = Self::split_parent(path)?;
                let parent = self.ensure_parent_chain(actor, &parent_path).await?;
                let spec = CreateFileSpec {
                    parent_path,
                    name,
                    kind: FileKind::File,
                    content_type: content_type.clone(),
                    visibility: None,
                    initial_content: None,
                };
                (self.create_in_parent(actor, &parent, spec).await?, false)
            }
            Err(err) => return Err(err),
        };

        // A file the caller may not even see must not be reported as "exists but forbidden".
        self.require_read(actor, &meta).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict(
                "cannot write to a collection".into(),
            ));
        }
        let perms = self.effective_permissions(actor, &meta).await?;
        if !perms.has(PermissionSet::WRITE) {
            return Err(MeiSyncError::Forbidden(format!(
                "write access denied: {path}"
            )));
        }
        let handle = self.documents.open(&meta).await?;
        let snapshot = if existed {
            handle
                .replace_content_checked(&actor.id, body, content_type.clone(), &preconditions)
                .await?
        } else {
            handle
                .replace_content(&actor.id, body, content_type.clone())
                .await?
        };
        let refreshed = self.files.find_by_id(&meta.id).await?.unwrap_or(meta);
        let path = self
            .path_of(&refreshed.id)
            .await
            .unwrap_or_else(|_| path.to_string());
        self.announce(&actor.id.0, ChangeKind::Updated, &refreshed, path, None);
        Ok(snapshot)
    }

    async fn apply_update(
        &self,
        actor: &User,
        id: &FileId,
        update: Vec<u8>,
        except: Option<meisync_document::ConnId>,
    ) -> Result<DocumentSnapshot> {
        let meta = self.get_by_id(actor, id).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict("cannot sync a collection".into()));
        }
        let perms = self.effective_permissions(actor, &meta).await?;
        if !perms.has(PermissionSet::SYNC) {
            return Err(MeiSyncError::Forbidden(format!(
                "sync access denied: {}",
                self.path_of(&meta.id).await.unwrap_or_default()
            )));
        }
        self.require_sync(actor, &meta).await?;
        let handle = self.documents.open(&meta).await?;
        let snapshot = handle.apply_update(&actor.id, update, except).await?;
        let refreshed = self.files.find_by_id(&meta.id).await?.unwrap_or(meta);
        let path = self.path_of(&refreshed.id).await.unwrap_or_default();
        self.announce(&actor.id.0, ChangeKind::Updated, &refreshed, path, None);
        Ok(snapshot)
    }

    async fn open_document(
        &self,
        actor: &User,
        path: &str,
    ) -> Result<(FileMetadata, DocumentHandle)> {
        let meta = self.resolve(actor, path).await?;
        self.require_read(actor, &meta).await?;
        if meta.is_collection() {
            return Err(MeiSyncError::Conflict("cannot sync a collection".into()));
        }
        let perms = self.effective_permissions(actor, &meta).await?;
        if !perms.has(PermissionSet::SYNC) {
            return Err(MeiSyncError::Forbidden(format!(
                "sync access denied: {path}"
            )));
        }
        let handle = self.documents.open(&meta).await?;
        Ok((meta, handle))
    }

    async fn path_of(&self, id: &FileId) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut current = self
            .files
            .find_by_id(id)
            .await?
            .ok_or_else(|| MeiSyncError::NotFound(id.0.clone()))?;
        loop {
            if current.id.as_str() == ROOT_ID {
                break;
            }
            parts.push(current.name.clone());
            match current.parent_id.clone() {
                Some(parent) => {
                    current = self
                        .files
                        .find_by_id(&parent)
                        .await?
                        .ok_or_else(|| MeiSyncError::internal("broken namespace link"))?;
                }
                None => break,
            }
        }
        parts.reverse();
        if parts.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", parts.join("/")))
        }
    }

    async fn set_visibility(
        &self,
        actor: &User,
        id: &FileId,
        visibility: Visibility,
    ) -> Result<()> {
        let meta = self
            .files
            .find_by_id(id)
            .await?
            .ok_or_else(|| MeiSyncError::NotFound(id.0.clone()))?;
        if meta.owner_id != actor.id {
            return Err(MeiSyncError::Forbidden(
                "only the owner may change visibility".into(),
            ));
        }
        self.files.set_visibility(id, visibility).await?;
        if let Some(refreshed) = self.files.find_by_id(id).await? {
            let path = self.path_of(id).await.unwrap_or_default();
            self.announce(&actor.id.0, ChangeKind::Updated, &refreshed, path, None);
        }
        Ok(())
    }

    async fn acl_list(&self, actor: &User, id: &FileId) -> Result<Vec<AclEntry>> {
        let meta = self.get_by_id(actor, id).await?;
        let perms = self.effective_permissions(actor, &meta).await?;
        if !perms.has(PermissionSet::DELETE) {
            return Err(MeiSyncError::Forbidden(
                "only the owner may read the ACL".into(),
            ));
        }
        self.acl.acl_list(id).await
    }

    async fn acl_grant(
        &self,
        actor: &User,
        id: &FileId,
        user: &meisync_core::UserId,
        permissions: PermissionSet,
    ) -> Result<()> {
        let meta = self
            .files
            .find_by_id(id)
            .await?
            .ok_or_else(|| MeiSyncError::NotFound(id.0.clone()))?;
        if meta.owner_id != actor.id {
            return Err(MeiSyncError::Forbidden(
                "only the owner may change the ACL".into(),
            ));
        }
        if permissions.has(PermissionSet::READ) && meta.visibility == Visibility::Private {
            // ACL grants never override the private namespace rule of a foreign document.
            return Err(MeiSyncError::Forbidden(
                "cannot grant access to a private file; make it public first".into(),
            ));
        }
        self.acl.acl_set(id, user, permissions).await
    }

    async fn list_recursive(&self, actor: &User, spec: &RecursiveListSpec<'_>) -> Result<Listing> {
        let base_path = spec.base_path;
        let prefix = spec.prefix;
        let max_keys = spec.max_keys;
        let start_after = spec.start_after.unwrap_or("");
        let delimiter = spec.delimiter.filter(|d| !d.is_empty());
        let mut objects: Vec<(String, FileMetadata)> = Vec::new();
        let mut prefixes: Vec<String> = Vec::new();
        let mut queue: Vec<FileMetadata> = vec![spec.root.clone()];

        while let Some(current) = queue.pop() {
            let mut children = self.files.list_children(&current.id).await?;
            children.sort_by(|a, b| a.name.cmp(&b.name));
            for child in children {
                if !self.permissions.can_read_metadata(actor, &child).await? {
                    continue;
                }
                let child_path = self.path_of(&child.id).await?;
                let key = relative_key(base_path, &child_path);

                // `key` matches when it is behind the prefix, or when the prefix points *into*
                // `key` (the collection still has to be descended into).
                let collection_path = format!("{key}/");
                if !key.starts_with(prefix) && !prefix.starts_with(&collection_path) {
                    continue;
                }

                // CommonPrefix handling: roll everything below the next delimiter up.
                if let Some(delimiter) = delimiter {
                    let start = prefix.len().min(key.len());
                    if let Some(offset) = key[start..].find(delimiter) {
                        let pos = start + offset;
                        let common = key[..pos + delimiter.len()].to_string();
                        if !prefixes.contains(&common) {
                            prefixes.push(common);
                        }
                        continue;
                    }
                }

                if child.is_collection() {
                    queue.push(child);
                } else {
                    objects.push((key, child));
                }
            }
        }

        objects.sort_by(|a, b| a.0.cmp(&b.0));
        prefixes.sort();

        // Objects and `CommonPrefixes` form ONE lexicographic sequence: they compete for the
        // same `MaxKeys` budget, so truncating the two lists separately would drop interleaved
        // objects (`a/1`, `b.txt`, `c/1`, `d.txt` with `delimiter=/`). `None` marks a prefix.
        let mut entries: Vec<(String, Option<FileMetadata>)> = objects
            .into_iter()
            .map(|(key, meta)| (key, Some(meta)))
            .chain(prefixes.into_iter().map(|common| (common, None)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Continuation: an S3 page resumes *after* the last element it returned, which is
        // either an object key or a prefix root.
        entries.retain(|(key, _)| key.as_str() > start_after);

        // `MaxKeys` counts keys *and* `CommonPrefixes` entries, exactly like S3. A page that
        // emits nothing (`max-keys=0`) is never truncated, so `IsTruncated` and the token stay
        // consistent.
        let truncated = max_keys > 0 && entries.len() > max_keys;
        entries.truncate(max_keys);
        let next_continuation_token = if truncated {
            entries.last().map(|(key, _)| key.clone())
        } else {
            None
        };

        let mut page_objects = Vec::new();
        let mut page_prefixes = Vec::new();
        for (key, meta) in entries {
            match meta {
                Some(meta) => page_objects.push(meta),
                None => page_prefixes.push(key),
            }
        }

        Ok(Listing {
            objects: page_objects,
            prefixes: page_prefixes,
            truncated,
            next_continuation_token,
        })
    }

    async fn bucket_collection(&self, bucket: &str) -> Result<FileMetadata> {
        if let Some(id) = self.buckets.find(bucket).await? {
            if let Some(meta) = self.files.find_by_id(&id).await? {
                return Ok(meta);
            }
        }
        // A collection created through WebDAV (or any other frontend) is a bucket as well:
        // buckets simply *are* the user owned top level collections of the namespace (§30).
        // The system owned `/private` container is a namespace, not a bucket.
        let root = root_id();
        match self.files.find_child(&root, bucket).await? {
            Some(meta) if is_bucket_candidate(&meta) => Ok(meta),
            _ => Err(MeiSyncError::NotFound(format!("no such bucket: {bucket}"))),
        }
    }

    async fn register_bucket(
        &self,
        name: &str,
        collection_id: &FileId,
        owner: &meisync_core::UserId,
    ) -> Result<()> {
        self.buckets.upsert(name, collection_id, Some(owner)).await
    }

    async fn buckets(&self) -> Result<Vec<(String, FileId, Option<String>)>> {
        let mut out: Vec<(String, FileId, Option<String>)> = self
            .buckets
            .list()
            .await?
            .into_iter()
            .map(|(name, id, owner)| (name, id, owner.map(|o| o.0)))
            .collect();
        // Include top level collections that were created without registering a bucket row.
        let root = root_id();
        for child in self.files.list_children(&root).await? {
            if !is_bucket_candidate(&child) {
                continue;
            }
            if !out.iter().any(|(name, _, _)| *name == child.name) {
                out.push((
                    child.name.clone(),
                    child.id.clone(),
                    Some(child.owner_id.0.clone()),
                ));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// A top level collection is addressable as an S3 bucket; the system owned namespace
/// containers (`/private`) are not.
fn is_bucket_candidate(meta: &FileMetadata) -> bool {
    meta.is_collection() && meta.owner_id.as_str() != SYSTEM_OWNER
}

/// The system owned `/private` container, whose children are provisioned per account.
fn is_private_root(meta: &FileMetadata) -> bool {
    meta.is_collection()
        && meta.name == PRIVATE_ROOT
        && meta.owner_id.as_str() == SYSTEM_OWNER
        && meta.parent_id.as_ref().map(|p| p.as_str() == ROOT_ID) == Some(true)
}

/// S3 key of a namespace path relative to the bucket collection.
fn relative_key(base_path: &str, path: &str) -> String {
    path.strip_prefix(base_path)
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.trim_start_matches('/').to_string())
}
