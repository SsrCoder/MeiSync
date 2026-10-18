//! Administration API and the embedded browser console.
//!
//! * `/api/admin/...` – JSON admin control plane, protected by the `admin` role.
//! * `/admin`         – a self-contained SPA (no build step, embedded into the binary).

pub mod repo;
pub mod routes;
pub mod web;

pub use repo::{
    directory_size, AdminDeviceRow, AdminRepository, AdminSessionRow, AdminStats, AdminUserRow,
};
pub use routes::{bootstrap, router};
pub use web::ui_router;
