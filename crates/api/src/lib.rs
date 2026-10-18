//! API / service layer of MeiSync.
//!
//! * [`service`] contains the single [`service::FileService`] used by *every* protocol
//!   (`docs/mvp.md` §33).
//! * [`state`] wires storage, `DocumentManager`, auth and the file service together (§35).
//! * [`routes`] is the small JSON control plane (`/api/...`).

pub mod error;
pub mod extract;
pub mod routes;
pub mod service;
pub mod state;

pub use error::{ApiError, FileInfo};
pub use extract::{AuthToken, AuthUser, MaybeAuthUser};
pub use service::{CreateFileSpec, DefaultFileService, Depth, FileService, Listing, RecursiveListSpec};
pub use state::{AppState, ServerConfig};