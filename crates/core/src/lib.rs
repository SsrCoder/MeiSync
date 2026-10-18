//! Core domain model for MeiSync.
//!
//! The whole system is built around a single invariant (see `docs/mvp.md` §2):
//!
//! > One File = One YDoc, and the YDoc is the only source of truth for the file content.
//!
//! This crate therefore contains:
//!
//! * [`file`] – the [`FileDocument`] aggregate: metadata + `yrs::Doc`.
//! * [`user`] – users and sessions.
//! * [`permission`] – the single permission decision point shared by every protocol
//!   (WebDAV / S3 / YSync) as required by `docs/mvp.md` §13.

pub mod acl;
pub mod error;
pub mod file;
pub mod permission;
pub mod precondition;
pub mod user;

pub use acl::{AclEntry, AclProvider, PermissionSet};
pub use error::{MeiSyncError, Result};
pub use file::{
    content_encoding, decode_content, encode_content, guess_content_type, guess_sync_mode,
    read_text, replace_content, resolve_content_type, ContentEncoding, FileDocument, FileId,
    FileKind, FileMetadata, SyncMode, TransactionOrigin, Visibility, CONTENT_KEY,
    GENERIC_CONTENT_TYPES,
};
pub use permission::{PermissionService, VisibilityPermissionService};
pub use precondition::{normalize_etag, Preconditions};
pub use user::{AuthToken, Credentials, PublicUser, Session, User, UserId};

/// The root of the file namespace is a virtual node: it is never persisted as a row.
pub const ROOT_PATH: &str = "/";
/// Top level collection holding per-user private namespaces.
pub const PRIVATE_ROOT: &str = "private";
/// Top level collection holding shared / public namespaces.
pub const PUBLIC_ROOT: &str = "public";