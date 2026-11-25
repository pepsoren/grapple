use crate::epic_auth::EpicApi;
use crate::manual::ManualEntry;
use crate::paths;
use crate::proc::ProcFs;
use crate::steam::SteamSettings;
use grapple_core::GameId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

/// When to run `steam -shutdown` after a Steam game ends (and no other Steam game runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseSteam {
    /// Default: only a Steam that Grapple started itself this session. A Steam you started yourself stays.
    #[default]
    WhenGrappleStartedIt,
    Always,
    Never,
}

/// Per-game wrapper choices from Grapple's own settings (GameMode, MangoHud). They apply to games Grapple starts
/// itself (Epic, manual). Steam starts its games, so Grapple cannot wrap them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wrappers {
    pub gamemode: bool,
    pub mangohud: bool,
}

#[derive(Debug, Clone)]
pub struct LaunchSettings {
    pub steam: SteamSettings,
    /// Start Steam hidden when Grapple starts. Off by default: it keeps about 1.3 GB of memory in use.
    pub keep_steam_ready: bool,
    /// Global default for "Close Steam when I quit".
    pub close_steam: CloseSteam,
    /// Per-game override: `true` = always close after this game, `false` = never.
    pub close_steam_per_game: HashMap<GameId, bool>,

    /// `~/.config/heroic`
    pub heroic_dir: PathBuf,
    /// `~/.config/heroic/legendaryConfig/legendary` (installed.json, user.json, metadata/)
    pub legendary_dir: PathBuf,
    /// The Epic Games Launcher's data (`%PROGRAMDATA%\Epic`, Windows only): games it installed start from here when
    /// legendary does not know them (see [`grapple_core::egl`]).
    pub egl_dir: Option<PathBuf>,
    /// `~/.steam/steam`, handed to Proton as `STEAM_COMPAT_CLIENT_INSTALL_PATH`.
    pub steam_client_path: PathBuf,
    /// Use this umu-run instead of searching `PATH` and Heroic's copy.
    pub umu_run: Option<PathBuf>,
    /// umu's folder with its Steam Linux Runtimes (`$XDG_DATA_HOME/umu`, see [`crate::umu::default_dir`]).
    pub umu_dir: PathBuf,
    /// Keep umu's runtimes current in the background while no game runs (at most daily), so launches never
    /// wait for umu's own update check. Off: launches still skip that check; umu then updates only when the
    /// runtime is missing or broken.
    pub runtime_updates: bool,
    pub epic_api: EpicApi,

    /// Wrapper choices per game. An Epic game without an entry keeps Heroic's own GameMode setting (no MangoHud);
    /// a manual game without one keeps its `ManualEntry` flags.
    pub wrappers: HashMap<GameId, Wrappers>,
    /// Per-game Proton (the `proton` script) chosen in grapple for Epic games, used before Heroic's choice
    /// (Linux; empty elsewhere). Heroic's `GamesConfig` is never written.
    pub proton_per_game: HashMap<GameId, PathBuf>,
    /// How controllers are shown to Proton games without their own choice (Settings › Controllers › In games).
    pub pad_mode: crate::pads::PadMode,
    /// Per-game choice of the same (Epic and manual games).
    pub pad_mode_per_game: HashMap<GameId, crate::pads::PadMode>,
    /// Epic games that start with Epic's in-game overlay (store checkouts show in it), see [`crate::eos`].
    pub eos_overlay_games: HashSet<GameId>,
    /// grapple's overlay install (`$XDG_DATA_HOME/grapple/eos-overlay`), once it exists.
    pub eos_overlay_dir: Option<PathBuf>,

    /// Manual games. The store (`Game`) does not carry launch details, so the app keeps them and passes them here.
    pub manual: HashMap<GameId, ManualEntry>,
    /// Proton used by Windows programs without their own `proton` (default: Heroic's configured default).
    pub default_proton: Option<PathBuf>,

    /// How often `/proc` is polled (idle). Tracked games are polled at most every 2 s.
    pub poll_interval: Duration,
    pub proc: ProcFs,
    /// Game logs (stdout/stderr of games Grapple starts): `$XDG_STATE_HOME/grapple/logs`.
    pub log_dir: PathBuf,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        let heroic_dir = paths::config_home().join("heroic");
        LaunchSettings {
            steam: SteamSettings::default(),
            keep_steam_ready: false,
            close_steam: CloseSteam::default(),
            close_steam_per_game: HashMap::new(),
            legendary_dir: heroic_dir.join("legendaryConfig").join("legendary"),
            egl_dir: grapple_core::egl::default_dir(),
            heroic_dir,
            steam_client_path: paths::home().join(".steam").join("steam"),
            umu_run: None,
            umu_dir: crate::umu::default_dir(),
            runtime_updates: true,
            epic_api: EpicApi::default(),
            wrappers: HashMap::new(),
            proton_per_game: HashMap::new(),
            pad_mode: crate::pads::PadMode::AsIs,
            pad_mode_per_game: HashMap::new(),
            eos_overlay_games: HashSet::new(),
            eos_overlay_dir: None,
            manual: HashMap::new(),
            default_proton: None,
            poll_interval: Duration::from_secs(5),
            proc: ProcFs::default(),
            log_dir: paths::log_dir(),
        }
    }
}

impl LaunchSettings {
    /// How controllers are shown to this game when it runs through Proton.
    pub fn pad_mode_for(&self, game: &GameId) -> crate::pads::PadMode {
        crate::pads::effective(self.pad_mode, self.pad_mode_per_game.get(game).copied())
    }

    /// Should Steam be shut down after `game` ended? `started_by_us`: Grapple started this Steam.
    pub fn should_close_steam(&self, game: &GameId, started_by_us: bool) -> bool {
        if self.keep_steam_ready {
            return false;
        }
        match self.close_steam_per_game.get(game) {
            Some(v) => *v,
            None => match self.close_steam {
                CloseSteam::Always => true,
                CloseSteam::Never => false,
                CloseSteam::WhenGrappleStartedIt => started_by_us,
            },
        }
    }
}
