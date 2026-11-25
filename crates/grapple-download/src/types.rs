//! The shapes the UI sees (`app/ui/src/lib/download-types.ts` mirrors them; serde output is camelCase).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DlStore {
    Steam,
    Epic,
}

impl DlStore {
    pub fn as_str(self) -> &'static str {
        match self {
            DlStore::Steam => "steam",
            DlStore::Epic => "epic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DlKind {
    Install,
    Update,
}

impl DlKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DlKind::Install => "install",
            DlKind::Update => "update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DlState {
    Queued,
    Starting,
    Downloading,
    Verifying,
    Installing,
    Paused,
    Done,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DlArt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DlItem {
    pub game: String,
    pub store: DlStore,
    pub kind: DlKind,
    pub state: DlState,
    pub name: String,
    pub art: DlArt,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub bytes_per_sec: Option<u64>,
    pub seconds_left: Option<u64>,
    /// Last 20 samples, bytes/s.
    pub speed_history: Vec<u64>,
    pub target_dir: String,
    pub position: i64,
    pub can_pause: bool,
    pub can_reorder: bool,
    pub can_cancel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeState {
    Off,
    On,
    Unavailable,
    RestartPending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DlSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<DlItem>,
    pub queued: Vec<DlItem>,
    pub updates: Vec<DlItem>,
    pub done: Vec<DlItem>,
    pub paused_all: bool,
    pub steam_bridge: BridgeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallTarget {
    /// Shown as is (`~/…` under the home folder); `install_start` takes it back.
    pub path: String,
    pub label: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub enough: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtonChoice {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub game: String,
    pub store: DlStore,
    pub size_bytes: Option<u64>,
    pub targets: Vec<InstallTarget>,
    /// Epic on Linux only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protons: Option<Vec<ProtonChoice>>,
}

/// Folders the settings page offers (read-only; ignored by `download_settings_set`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirChoices {
    pub steam: Vec<String>,
    pub epic: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DlSettings {
    /// 0 = no limit.
    pub speed_limit_bytes: u64,
    pub connections: u32,
    pub auto_update: bool,
    pub update_only_when_idle: bool,
    pub pause_while_playing: bool,
    pub notify_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steam_default_dir: Option<String>,
    pub epic_default_dir: String,
    pub steam_bridge: bool,
    /// The loopback port Steam's DevTools listen on for the bridge (`-devtools-port`), picked once when the bridge
    /// is first turned on. 0: not picked yet.
    pub steam_bridge_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<DirChoices>,
}

impl Default for DlSettings {
    fn default() -> Self {
        DlSettings {
            speed_limit_bytes: 0,
            connections: 8,
            auto_update: true,
            update_only_when_idle: true,
            pause_while_playing: true,
            notify_ready: true,
            steam_default_dir: None,
            epic_default_dir: String::new(),
            steam_bridge: false,
            steam_bridge_port: 0,
            choices: None,
        }
    }
}

/// The answer to "Check for update" on one game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateCheck {
    UpToDate,
    /// An update exists and waits for "Update now" (automatic updates are off).
    Available,
    /// An update exists and is in the queue.
    Queued,
    /// The update is already downloading (or paused part way).
    Downloading,
}
