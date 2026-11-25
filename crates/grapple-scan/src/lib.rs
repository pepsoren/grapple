//! Finds games: Steam (local files, no login), Epic via Heroic/legendary files, and (later) manual entries from the Db.
//!
//! `scan_all` is synchronous and quick (tens of milliseconds); call it from `spawn_blocking`.
//! It only reads: nothing under the Steam or Heroic directories is ever written.

pub mod appinfo;
mod epic;
mod steam;
pub mod vdf;

pub use steam::{flags as steam_flags, library_dirs as steam_library_dirs, status_from_flags as steam_status_from_flags};

use grapple_core::Game;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ScanConfig {
    /// Steam's data directory (the one holding `appcache/` and `steamapps/`).
    pub steam_root: Option<PathBuf>,
    /// Heroic's config directory (`~/.config/heroic`): images-cache, store/timestamp.json, GamesConfig.
    pub heroic_config: Option<PathBuf>,
    /// legendary's config directory (`.../legendary` with installed.json and metadata/). Defaults to the one
    /// inside Heroic's config; standalone legendary (`~/.config/legendary`) is used when Heroic has none.
    pub legendary_dir: Option<PathBuf>,
    /// The Epic Games Launcher's data folder (`%PROGRAMDATA%\Epic`, Windows): the games it installed are added.
    pub egl_dir: Option<PathBuf>,
}

impl ScanConfig {
    /// Look in the usual places (XDG paths, the `~/.steam/steam` symlink, the Steam and Heroic flatpaks).
    #[cfg(not(windows))]
    pub fn detect() -> ScanConfig {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| -> Option<PathBuf> {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
        };
        let data = xdg("XDG_DATA_HOME", ".local/share");
        let config = xdg("XDG_CONFIG_HOME", ".config");

        let mut steam_candidates = Vec::new();
        if let Some(d) = &data {
            steam_candidates.push(d.join("Steam"));
        }
        if let Some(h) = &home {
            steam_candidates.push(h.join(".steam/steam"));
            steam_candidates.push(h.join(".steam/root"));
            steam_candidates.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
        let steam_root = steam_candidates.into_iter().find(|p| p.join("steamapps").is_dir() || p.join("appcache").is_dir());

        let mut heroic_candidates = Vec::new();
        if let Some(c) = &config {
            heroic_candidates.push(c.join("heroic"));
        }
        if let Some(h) = &home {
            heroic_candidates.push(h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"));
        }
        let heroic_config = heroic_candidates.into_iter().find(|p| p.is_dir());

        let mut legendary_dir = heroic_config
            .as_ref()
            .map(|h| h.join("legendaryConfig/legendary"))
            .filter(|p| p.join("installed.json").exists() || p.join("metadata").is_dir());
        if legendary_dir.is_none() {
            legendary_dir = config.map(|c| c.join("legendary")).filter(|p| p.is_dir());
        }
        ScanConfig { steam_root, heroic_config, legendary_dir, egl_dir: None }
    }

    /// Windows: Steam's folder from the registry, Heroic in `%APPDATA%\heroic`, standalone legendary in
    /// `%USERPROFILE%\.config\legendary` (legendary uses `~/.config` on every system), and the Epic Games Launcher.
    #[cfg(windows)]
    pub fn detect() -> ScanConfig {
        let steam_root = grapple_core::winreg::steam_dir().filter(|p| p.join("steamapps").is_dir() || p.join("appcache").is_dir());
        let heroic_config = Some(grapple_core::dirs::config_home().join("heroic")).filter(|p| p.is_dir());
        let legendary_config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| grapple_core::dirs::home().join(".config"))
            .join("legendary");
        let legendary_dir = heroic_config
            .as_ref()
            .map(|h| h.join("legendaryConfig").join("legendary"))
            .filter(|p| p.join("installed.json").exists() || p.join("metadata").is_dir())
            .or_else(|| Some(legendary_config).filter(|p| p.is_dir()));
        ScanConfig { steam_root, heroic_config, legendary_dir, egl_dir: grapple_core::egl::default_dir().filter(|d| d.is_dir()) }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// Steam then Epic, each sorted by `sort_name`.
    pub games: Vec<Game>,
    /// Things that were missing or unreadable; the scan carries on without them.
    pub warnings: Vec<String>,
}

/// Scan every configured source. Never fails: problems become warnings.
pub fn scan_all(cfg: &ScanConfig) -> ScanResult {
    let mut res = ScanResult::default();
    match &cfg.steam_root {
        Some(root) => res.games.extend(steam::scan(root, &mut res.warnings)),
        None => res.warnings.push("steam: not found".into()),
    }
    let mut epic_games = match &cfg.legendary_dir {
        Some(leg) => epic::scan(leg, cfg.heroic_config.as_deref(), &mut res.warnings),
        None => Vec::new(),
    };
    if let Some(dir) = &cfg.egl_dir {
        epic::add_egl(&mut epic_games, grapple_core::egl::installed(dir));
    }
    res.games.extend(epic_games);
    res
}
