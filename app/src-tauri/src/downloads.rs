//! Downloads glue (real backend only): the `grapple_download::Host` the downloads service talks to, its setup, and
//! the Proton list for "Runs with". The service itself is the grapple-download crate; the commands are thin
//! wrappers in `commands.rs` over `Backend` methods.

use crate::AppState;
use futures_util::future::BoxFuture;
use grapple_download::{Config, DlSnapshot, Downloads, GameInfo, Host, ProtonChoice};
use grapple_launch::Launcher;
use grapple_launch::proc::ProcFs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Where Heroic, legendary and Steam are, as the scanner finds them (with grapple-launch's defaults).
pub struct Places {
    pub legendary_dir: PathBuf,
    pub heroic_dir: PathBuf,
    pub steam_root: Option<PathBuf>,
}

pub fn places() -> Places {
    let scan = grapple_scan::ScanConfig::detect();
    let d = grapple_launch::LaunchSettings::default();
    Places { legendary_dir: scan.legendary_dir.unwrap_or(d.legendary_dir), heroic_dir: scan.heroic_config.unwrap_or(d.heroic_dir), steam_root: scan.steam_root }
}

/// Every Proton grapple can run an Epic game with, and Heroic's own default (its folder name).
pub fn proton_list() -> (Vec<grapple_launch::proton::ProtonVersion>, Option<String>) {
    let p = places();
    let home = grapple_launch::paths::home();
    let mut roots: Vec<PathBuf> = p.steam_root.iter().cloned().collect();
    for r in [home.join(".steam/root"), grapple_launch::paths::data_home().join("Steam")] {
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    let libs = p.steam_root.as_deref().map(grapple_scan::steam_library_dirs).unwrap_or_default();
    let list = grapple_launch::proton::find(&p.heroic_dir, &roots, &libs);
    let heroic_default = grapple_launch::heroic::HeroicSettings::load(&p.heroic_dir, "_grapple_none_")
        .ok()
        .and_then(|h| PathBuf::from(h.wine_version.bin).parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()));
    (list, heroic_default)
}

struct DlHost {
    app: AppHandle,
    launcher: Launcher,
}

impl Host for DlHost {
    fn emit(&self, snap: &DlSnapshot) {
        let _ = self.app.emit("downloads-changed", snap);
    }

    fn game(&self, id: &str) -> Option<GameInfo> {
        self.app.try_state::<Arc<AppState>>()?.backend.dl_game(id)
    }

    fn games_changed(&self) {
        let Some(state) = self.app.try_state::<Arc<AppState>>() else { return };
        let state = state.inner().clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = state.backend.rescan().await {
                tracing::warn!("rescan after a download: {e}");
            }
        });
    }

    fn notify_ready(&self, name: &str) {
        crate::store::notify_desktop(format!("{name} is ready"), String::new());
    }

    fn steam_game_running(&self) -> bool {
        self.launcher.running().iter().any(|(g, _)| g.0.starts_with("steam:"))
    }

    fn steam_open_url(&self, url: &str) -> grapple_download::Result<()> {
        // Like every Steam command: session environment, outside grapple's process tree.
        self.launcher.settings().steam.open_url(url).map_err(Into::into)
    }

    fn restart_steam(&self) -> BoxFuture<'static, grapple_download::Result<()>> {
        let steam = self.launcher.settings().steam.clone();
        Box::pin(async move {
            let fs = ProcFs::default();
            if !steam.is_running(&fs) {
                return Ok(());
            }
            steam.shutdown(&fs)?;
            let t = Instant::now();
            while steam.is_running(&fs) {
                if t.elapsed() > Duration::from_secs(60) {
                    return Err(grapple_download::Error::Steam("Steam did not quit".into()));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            // Hidden again, the way "Keep Steam ready" starts it.
            steam.start_ready(&fs, &AtomicBool::new(false)).await?;
            Ok(())
        })
    }

    fn set_steam_devtools_port(&self, port: Option<u16>) {
        let mut s = (*self.launcher.settings()).clone();
        s.steam.flags.devtools_port = port;
        self.launcher.update_settings(s);
    }

    fn protons(&self) -> Vec<ProtonChoice> {
        if !cfg!(target_os = "linux") {
            return Vec::new();
        }
        let (list, heroic_default) = proton_list();
        grapple_launch::proton::choices(&list, heroic_default.as_deref()).into_iter().map(|(name, is_default)| ProtonChoice { name, is_default }).collect()
    }
}

/// Create the downloads service. `None` (with a warning) when its database cannot be opened: grapple runs without
/// downloads then.
pub fn init(app: &AppHandle, launcher: Launcher) -> Option<Downloads> {
    let p = places();
    let db_path = grapple_core::db::default_db_path().ok()?.parent()?.join("downloads.db");
    let auth = match grapple_launch::epic_auth::EpicAuth::new(grapple_launch::epic_auth::EpicApi::default(), p.legendary_dir.join("user.json")) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("downloads: no Epic client ({e})");
            return None;
        }
    };
    let cfg = match Config::new(db_path, p.legendary_dir, p.heroic_dir, p.steam_root, grapple_launch::paths::home(), Arc::new(auth)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("downloads: {e}");
            return None;
        }
    };
    match Downloads::new(cfg, Arc::new(DlHost { app: app.clone(), launcher })) {
        Ok(d) => {
            let started = d.clone();
            tauri::async_runtime::spawn(async move { started.start() });
            Some(d)
        }
        Err(e) => {
            tracing::warn!("downloads: off ({e})");
            None
        }
    }
}
