use async_trait::async_trait;

use crate::acl::{AclProvider, PermissionSet};
use crate::error::Result;
use crate::file::{FileDocument, FileKind, FileMetadata, Visibility};
use crate::user::User;

/// The single permission decision point shared by WebDAV, S3 and YSync
/// (`docs/mvp.md` §13).
///
/// Protocol handlers must never implement permission logic themselves; they always go through
/// `FileService -> PermissionService`.
#[async_trait]
pub trait PermissionService: Send + Sync {
    async fn can_read(&self, user: &User, file: &FileDocument) -> Result<bool>;
    async fn can_write(&self, user: &User, file: &FileDocument) -> Result<bool>;
    async fn can_sync(&self, user: &User, file: &FileDocument) -> Result<bool>;
    async fn can_delete(&self, user: &User, file: &FileDocument) -> Result<bool>;

    /// Capability of the *containing collection* required to create a child named `name`.
    async fn can_create_child(
        &self,
        user: &User,
        parent: &FileMetadata,
    ) -> Result<bool>;

    /// Convenience: metadata only variant, used by directory listings.
    async fn can_read_metadata(&self, user: &User, meta: &FileMetadata) -> Result<bool> {
        self.permissions_for(user, meta).await.map(|p| p.has(PermissionSet::READ))
    }

    async fn can_write_metadata(&self, user: &User, meta: &FileMetadata) -> Result<bool> {
        self.permissions_for(user, meta).await.map(|p| p.has(PermissionSet::WRITE))
    }

    async fn can_delete_metadata(&self, user: &User, meta: &FileMetadata) -> Result<bool> {
        self.permissions_for(user, meta).await.map(|p| p.has(PermissionSet::DELETE))
    }

    async fn can_sync_metadata(&self, user: &User, meta: &FileMetadata) -> Result<bool> {
        self.permissions_for(user, meta).await.map(|p| p.has(PermissionSet::SYNC))
    }

    /// Effective permission set of `user` on a document.
    async fn permissions_for(&self, user: &User, meta: &FileMetadata) -> Result<PermissionSet>;
}

/// `visibility` (`docs/mvp.md` §12) + ACL grants.
pub struct VisibilityPermissionService<A: AclProvider> {
    acl: A,
}

impl<A: AclProvider> VisibilityPermissionService<A> {
    pub fn new(acl: A) -> Self {
        Self { acl }
    }
}

#[async_trait]
impl<A: AclProvider> PermissionService for VisibilityPermissionService<A> {
    async fn permissions_for(&self, user: &User, meta: &FileMetadata) -> Result<PermissionSet> {
        if meta.owner_id == user.id {
            return Ok(PermissionSet::ALL);
        }

        match meta.visibility {
            Visibility::Private => Ok(PermissionSet::NONE),
            // Public: every authenticated user may read + sync, mutations require a grant.
            Visibility::Public => {
                let granted = self.acl.acl_permissions(&meta.id, &user.id).await?;
                Ok(PermissionSet::READ
                    .union(PermissionSet::SYNC)
                    .union(granted))
            }
        }
    }

    async fn can_read(&self, user: &User, file: &FileDocument) -> Result<bool> {
        self.permissions_for(user, &file.metadata)
            .await
            .map(|p| p.has(PermissionSet::READ))
    }

    async fn can_write(&self, user: &User, file: &FileDocument) -> Result<bool> {
        self.permissions_for(user, &file.metadata)
            .await
            .map(|p| p.has(PermissionSet::WRITE))
    }

    async fn can_sync(&self, user: &User, file: &FileDocument) -> Result<bool> {
        self.permissions_for(user, &file.metadata)
            .await
            .map(|p| p.has(PermissionSet::SYNC))
    }

    async fn can_delete(&self, user: &User, file: &FileDocument) -> Result<bool> {
        self.permissions_for(user, &file.metadata)
            .await
            .map(|p| p.has(PermissionSet::DELETE))
    }

    /// Creating a child requires write access on the parent collection.
    async fn can_create_child(&self, user: &User, parent: &FileMetadata) -> Result<bool> {
        if parent.kind != FileKind::Collection {
            return Ok(false);
        }
        self.permissions_for(user, parent)
            .await
            .map(|p| p.has(PermissionSet::WRITE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{AclEntry, PermissionSet};
    use crate::error::Result as CResult;
    use crate::file::{FileId, SyncMode};
    use crate::user::UserId;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::collections::HashMap;

    struct MemAcl {
        map: std::sync::Mutex<HashMap<(String, String), PermissionSet>>,
    }

    impl MemAcl {
        fn new() -> Self {
            Self {
                map: std::sync::Mutex::new(HashMap::new()),
            }
        }

        fn grant(&self, file: &str, user: &str, perms: PermissionSet) {
            self.map
                .lock()
                .unwrap()
                .insert((file.to_string(), user.to_string()), perms);
        }
    }

    #[async_trait]
    impl AclProvider for MemAcl {
        async fn acl_permissions(
            &self,
            file_id: &FileId,
            user_id: &UserId,
        ) -> CResult<PermissionSet> {
            let map = self.map.lock().unwrap();
            Ok(map
                .get(&(file_id.0.clone(), user_id.0.clone()))
                .copied()
                .unwrap_or(PermissionSet::NONE))
        }
        async fn acl_list(&self, _file_id: &FileId) -> CResult<Vec<AclEntry>> {
            Ok(Vec::new())
        }
        async fn acl_set(
            &self,
            file_id: &FileId,
            user_id: &UserId,
            permissions: PermissionSet,
        ) -> CResult<()> {
            self.grant(&file_id.0, &user_id.0, permissions);
            Ok(())
        }
    }

    fn meta(visibility: Visibility, owner: &str) -> FileMetadata {
        FileMetadata {
            id: FileId::new("f1"),
            parent_id: None,
            name: "a.md".into(),
            owner_id: UserId::new(owner),
            visibility,
            content_type: None,
            kind: FileKind::File,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            size: 0,
            etag: None,
            version: 1,
            sync_mode: SyncMode::Incremental,
        }
    }

    fn user(name: &str) -> User {
        User {
            id: UserId::new(name),
            username: name.into(),
            password_hash: "x".into(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn owner_has_all_permissions() {
        let svc = VisibilityPermissionService::new(MemAcl::new());
        let m = meta(Visibility::Private, "alice");
        let perms = svc.permissions_for(&user("alice"), &m).await.unwrap();
        assert_eq!(perms, PermissionSet::ALL);
    }

    #[tokio::test]
    async fn private_is_invisible_for_other_users() {
        let svc = VisibilityPermissionService::new(MemAcl::new());
        let m = meta(Visibility::Private, "alice");
        let perms = svc.permissions_for(&user("bob"), &m).await.unwrap();
        assert!(perms.is_empty());
    }

    #[tokio::test]
    async fn public_grants_read_and_sync_only() {
        let svc = VisibilityPermissionService::new(MemAcl::new());
        let m = meta(Visibility::Public, "alice");
        let perms = svc.permissions_for(&user("bob"), &m).await.unwrap();
        assert!(perms.has(PermissionSet::READ));
        assert!(perms.has(PermissionSet::SYNC));
        assert!(!perms.has(PermissionSet::WRITE));
    }

    #[tokio::test]
    async fn acl_grants_write_on_public_files() {
        let acl = MemAcl::new();
        acl.grant("f1", "bob", PermissionSet::WRITE);
        let svc = VisibilityPermissionService::new(acl);
        let m = meta(Visibility::Public, "alice");
        let perms = svc.permissions_for(&user("bob"), &m).await.unwrap();
        assert!(perms.has(PermissionSet::WRITE));
    }
}