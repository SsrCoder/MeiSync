//! Document layer of MeiSync: the `DocumentManager` keeps exactly one YDoc per FileDocument,
//! loads / caches / persists it and broadcasts updates to the YSync rooms.
//!
//! See `docs/mvp.md` §21 – §24 and §34.

pub mod loaded;
pub mod manager;
pub mod room;

pub use loaded::{
    empty_document, restore_document, state_etag, DocumentHandle, DocumentSnapshot, LoadedDocument,
};
pub use manager::{DocumentManager, DocumentManagerConfig, DocumentStatus};
pub use room::{ConnId, Room, RoomEvent};