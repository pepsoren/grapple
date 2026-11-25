//! Downloads for grapple (see `docs/DOWNLOADS_PLAN.md`).
//!
//! * Epic games are downloaded by grapple itself: [`manifest`] and [`chunk`] read Epic's formats, [`epic`] talks
//!   to Epic's launcher service and CDN, [`engine`] installs, updates and verifies, and [`legendary`] writes the
//!   files legendary and Heroic read (`installed.json`, `manifests/`).
//! * Steam games are downloaded by Steam. [`steam_watch`] follows its files; the opt-in [`steam_bridge`] controls
//!   it through Steam's own UI process (Chrome DevTools Protocol on loopback).
//! * [`queue`] is the one list of both, kept in its own SQLite file (`downloads.db`), and [`service`] is what the
//!   app holds.

pub mod chunk;
pub mod engine;
pub mod eos_overlay;
pub mod epic;
pub mod error;
pub mod fsutil;
pub mod legendary;
pub mod manifest;
pub mod queue;
pub mod ratelimit;
pub mod service;
pub mod steam_bridge;
pub mod steam_watch;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use error::{Error, Result};
pub use service::{Config, Downloads, GameInfo, Host};
pub mod types;
pub use types::*;
