use serde::{Deserialize, Serialize};

use crate::file::FileId;
use crate::user::UserId;

/// Bit set of capabilities that can be granted on a single FileDocument.
///
/// `docs/mvp.md` §12 states that public files are readable / writable / syncable by every
/// authenticated user by default, "具体 Write 权限仍由 File Permission 层控制" – this struct
/// *is* that File Permission layer. Read + Sync are granted to every authenticated user for
/// public files; Write / Delete stay with the owner unless explicitly granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionSet(pub u8);

impl PermissionSet {
    pub const NONE: PermissionSet = PermissionSet(0b0000);
    pub const READ: PermissionSet = PermissionSet(0b0001);
    pub const WRITE: PermissionSet = PermissionSet(0b0010);
    pub const SYNC: PermissionSet = PermissionSet(0b0100);
    pub const DELETE: PermissionSet = PermissionSet(0b1000);
    pub const ALL: PermissionSet = PermissionSet(0b1111);

    pub fn has(&self, other: PermissionSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: PermissionSet) -> PermissionSet {
        PermissionSet(self.0 | other.0)
    }

    pub fn insert(&mut self, other: PermissionSet) {
        self.0 |= other.0;
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    pub fn to_list(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.has(PermissionSet::READ) {
            out.push("read");
        }
        if self.has(PermissionSet::WRITE) {
            out.push("write");
        }
        if self.has(PermissionSet::SYNC) {
            out.push("sync");
        }
        if self.has(PermissionSet::DELETE) {
            out.push("delete");
        }
        out
    }

    pub fn from_list<I, S>(items: I) -> PermissionSet
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = PermissionSet::NONE;
        for item in items {
            match item.as_ref().trim().to_ascii_lowercase().as_str() {
                "read" => set.insert(PermissionSet::READ),
                "write" => set.insert(PermissionSet::WRITE),
                "sync" => set.insert(PermissionSet::SYNC),
                "delete" => set.insert(PermissionSet::DELETE),
                "all" => set.insert(PermissionSet::ALL),
                _ => {}
            }
        }
        set
    }
}

impl std::ops::BitOr for PermissionSet {
    type Output = PermissionSet;
    fn bitor(self, rhs: PermissionSet) -> PermissionSet {
        PermissionSet(self.0 | rhs.0)
    }
}

/// A single row of the `acl` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub file_id: FileId,
    pub user_id: UserId,
    pub permissions: PermissionSet,
}

/// Persistence hook used by [`crate::permission::PermissionService`]. Implemented by the
/// SQLite backed store so that permission checks stay a single implementation for all protocols.
#[async_trait::async_trait]
pub trait AclProvider: Send + Sync {
    /// Effective grants of `user` on `file_id` (union of all matching rows).
    async fn acl_permissions(&self, file_id: &FileId, user_id: &UserId) -> crate::error::Result<PermissionSet>;

    /// All ACL rows of a document (owner / administration UI).
    async fn acl_list(&self, file_id: &FileId) -> crate::error::Result<Vec<AclEntry>>;

    /// Grant / revoke permissions.
    async fn acl_set(
        &self,
        file_id: &FileId,
        user_id: &UserId,
        permissions: PermissionSet,
    ) -> crate::error::Result<()>;
}

/// Blanket implementation so repositories can be shared as `Arc<..>`.
#[async_trait::async_trait]
impl<T> AclProvider for std::sync::Arc<T>
where
    T: AclProvider + ?Sized + Send + Sync,
{
    async fn acl_permissions(
        &self,
        file_id: &FileId,
        user_id: &UserId,
    ) -> crate::error::Result<PermissionSet> {
        (**self).acl_permissions(file_id, user_id).await
    }

    async fn acl_list(&self, file_id: &FileId) -> crate::error::Result<Vec<AclEntry>> {
        (**self).acl_list(file_id).await
    }

    async fn acl_set(
        &self,
        file_id: &FileId,
        user_id: &UserId,
        permissions: PermissionSet,
    ) -> crate::error::Result<()> {
        (**self).acl_set(file_id, user_id, permissions).await
    }
}