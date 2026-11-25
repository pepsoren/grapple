//! Grapple's game store: the data side.
//!
//! Steam and Epic data (public endpoints only), cached in SQLite and refreshed in the background; prices in
//! the display currency; filters; the wishlist and sale alerts; explainable recommendations. See
//! `docs/STORE_PLAN.md`. The Tauri commands live in the app (`app/src-tauri/src/store*.rs`).

pub mod art;
pub mod db;
pub mod epic;
pub mod epic_account;
pub mod error;
pub mod filters;
pub mod fx;
pub mod local;
pub mod money;
pub mod net;
pub mod reco;
pub mod secrets;
pub mod service;
pub mod steam;
pub mod types;

pub use error::{Error, Result};
pub use money::Money;
pub use service::{BuyTarget, StoreConfig, StoreService};
pub use types::*;
