//! Shared types and the local store. See PLAN.md.

pub mod db;
pub mod dirs;
pub mod egl;
pub mod model;
pub mod names;
pub mod winreg;

pub use db::Db;
pub use model::*;
pub use names::sort_name;
