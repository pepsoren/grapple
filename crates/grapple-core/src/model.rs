//! The shapes every crate and the UI share. Change them only with the lead: the UI mirrors them in
//! `app/ui/src/lib/types.ts`, and the store persists them.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a game comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    Steam,
    Epic,
    Manual,
}

/// Stable id: `steam:<appid>`, `epic:<app_name>`, `manual:<uuid>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameId(pub String);

impl GameId {
    pub fn new(store: Store, store_id: &str) -> Self {
        let prefix = match store {
            Store::Steam => "steam",
            Store::Epic => "epic",
            Store::Manual => "manual",
        };
        GameId(format!("{prefix}:{store_id}"))
    }

    /// The part after the colon (Steam appid, Epic app_name, manual uuid).
    pub fn store_id(&self) -> &str {
        self.0.split_once(':').map_or(&self.0, |(_, id)| id)
    }
}

/// Local image files for a game. Any of them may be missing; the UI falls back
/// (no hero: blurred cover; no logo: the name as text; no icon: first letter).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Art {
    /// Portrait 2:3 cover.
    pub cover: Option<PathBuf>,
    /// Wide background art.
    pub hero: Option<PathBuf>,
    /// Transparent logo.
    pub logo: Option<PathBuf>,
    /// Small square icon (32px for Steam).
    pub icon: Option<PathBuf>,
}

/// Something the UI should show next to the game in colour (Steam's "Update queued" and friends).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GameStatus {
    UpdateQueued,
    Updating,
    Running,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Game {
    pub id: GameId,
    pub store: Store,
    pub name: String,
    /// Lowercased name without articles/punctuation, for sorting and search.
    pub sort_name: String,
    pub installed: bool,
    pub install_dir: Option<PathBuf>,
    pub size_bytes: Option<u64>,
    pub art: Art,
    /// The store's own total, in minutes (Steam's Playtime, Heroic's timestamp.json). 0 if unknown.
    pub store_playtime_min: u64,
    /// Unix seconds, the store's or ours, whichever is newer.
    pub last_played: Option<i64>,
    pub status: Option<GameStatus>,
}

/// Who noticed a play session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    /// Grapple started the game.
    Grapple,
    /// Grapple saw it running (started from Steam or elsewhere).
    Detected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub game: GameId,
    /// Unix seconds.
    pub started_at: i64,
    /// None while the game is running.
    pub ended_at: Option<i64>,
    pub source: SessionSource,
}

/// Per-game totals the UI shows (computed by the store from sessions + store playtime).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayStats {
    /// Max(store total, our session total): never the sum (Steam counts our sessions too).
    pub total_min: u64,
    pub sessions: u64,
    pub longest_min: u64,
    pub this_week_min: u64,
    /// Minutes per day for the last 14 days, oldest first.
    pub last_14_days: [u32; 14],
}

/// What happens while a game starts; the UI shows these as the launch steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LaunchEvent {
    Step { label: String, done: bool },
    Running { game: GameId, started_at: i64 },
    Exited { game: GameId, ended_at: i64 },
    Failed { game: GameId, message: String },
}
