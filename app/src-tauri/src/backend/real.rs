//! The real backend: grapple-core `Db` (SQLite cache), grapple-scan, grapple-launch, grapple-tether.
//! Enabled with the `real` feature (the default).
//!
//! Start-up order (speed rule): `list_games` answers from the SQLite cache at once; `start()` then
//! rescans in the background, writes the result with `replace_games` and emits `games-changed`.
//! The tether service connects as soon as the backend exists (if a sign-in is stored), so presence works
//! even when Grapple starts hidden at login.

use super::*;
use crate::invites::{InvitesSnapshot, LibGame, PlayAction, PlayOutcome, SendResult, decide};
use crate::manual_games::{self, Detected, ManualGame};
use crate::nowplaying::{Playing, art_source, choose_playing};
use crate::tether_svc::{Deps, PlayRequest, TetherService};
use anyhow::{Context, Result, anyhow};
use grapple_core::{Db, Game, GameId, LaunchEvent};
use grapple_launch::manual::ManualEntry;
use grapple_launch::{LaunchSettings, Launcher, Wrappers};
use grapple_scan::{ScanConfig, scan_all};
use grapple_tether::{MemoryStore, Tether, TetherConfig};
use std::collections::HashMap as Map;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

const SETTINGS_KEY: &str = "app.settings";
/// Unix seconds of the last finished rescan (Settings › Library › Rescan libraries).
const LAST_SCAN_KEY: &str = "app.last_scan";

fn game_settings_key(id: &str) -> String {
    format!("app.game_settings.{id}")
}

/// What the launcher needs besides the global settings, worked out from the Db.
#[derive(Debug, Default, PartialEq)]
pub struct LaunchMaps {
    pub manual: Map<GameId, ManualEntry>,
    pub close_steam_per_game: Map<GameId, bool>,
    pub wrappers: Map<GameId, Wrappers>,
    /// Epic games with a Proton picked in grapple (Linux).
    pub proton: Map<GameId, std::path::PathBuf>,
    /// Epic games that start with Epic's in-game overlay.
    pub eos_overlay: std::collections::HashSet<GameId>,
    /// Epic and manual games with their own "Controller" choice (Linux, Proton).
    pub pad_mode: Map<GameId, grapple_launch::pads::PadMode>,
}

/// Per-game choices -> launcher settings. Steam games only carry "Close Steam when I quit" (Steam starts the
/// game, so Grapple cannot wrap it), and only when the game has its own choice; Epic and manual games get their
/// GameMode / MangoHud switches (the defaults in `settings` when the game has none) and Epic games their Proton
/// (the game's own, else the default one, else Heroic's).
pub fn launch_maps(db: &Db, games: &[Game], manual: &[ManualGame], settings: &Settings) -> LaunchMaps {
    let mut maps = LaunchMaps { manual: manual_games::launch_map(manual), ..LaunchMaps::default() };
    // Looked up once, and only when some game has a choice.
    let mut protons: Option<Vec<grapple_launch::proton::ProtonVersion>> = None;
    for g in games {
        let saved: Option<GameSettings> = db.setting(&game_settings_key(&g.id.0)).ok().flatten();
        match g.store {
            Store::Steam => {
                if let Some(v) = saved.and_then(|s| s.close_steam_on_quit) {
                    maps.close_steam_per_game.insert(g.id.clone(), v);
                }
            }
            Store::Epic | Store::Manual => {
                let s = saved.unwrap_or_default();
                let wrappers = Wrappers {
                    gamemode: s.gamemode.unwrap_or(settings.default_gamemode),
                    mangohud: s.mangohud.unwrap_or(settings.default_mangohud),
                };
                maps.wrappers.insert(g.id.clone(), wrappers);
                if g.store == Store::Epic && s.eos_overlay {
                    maps.eos_overlay.insert(g.id.clone());
                }
                if let Some(m) = s.pad_in_games.as_deref() {
                    maps.pad_mode.insert(g.id.clone(), grapple_launch::pads::PadMode::parse(m));
                }
                if cfg!(target_os = "linux")
                    && g.store == Store::Epic
                    && let Some(name) = s.proton.as_deref().or(settings.default_proton.as_deref())
                {
                    let list = protons.get_or_insert_with(|| crate::downloads::proton_list().0);
                    if let Some(bin) = grapple_launch::proton::resolve(name, list) {
                        maps.proton.insert(g.id.clone(), bin);
                    }
                }
            }
        }
    }
    maps
}

/// The global launch choices from the app settings.
fn apply_defaults(s: &Settings, l: &mut LaunchSettings) {
    l.pad_mode = grapple_launch::pads::PadMode::parse(&s.pad_in_games);
    l.keep_steam_ready = s.keep_steam_ready;
    l.runtime_updates = s.update_runtime;
    l.close_steam = if s.close_steam { grapple_launch::CloseSteam::WhenGrappleStartedIt } else { grapple_launch::CloseSteam::Never };
    // Windows programs added by hand run with the default Proton (else Heroic's, the launcher's own fallback).
    l.default_proton = match s.default_proton.as_deref() {
        Some(name) if cfg!(target_os = "linux") => grapple_launch::proton::resolve(name, &crate::downloads::proton_list().0),
        _ => None,
    };
}

/// Make the tether client: tokens in the system keyring. `GRAPPLE_NO_KEYRING=1` keeps them in memory only
/// (testing the app without touching the keyring; the sign-in is forgotten at exit).
fn tether_factory() -> Arc<dyn Fn(TetherConfig) -> grapple_tether::Result<Tether> + Send + Sync> {
    if std::env::var_os("GRAPPLE_NO_KEYRING").is_some() {
        let store = Arc::new(MemoryStore::new());
        Arc::new(move |c| Tether::new(c, store.clone()))
    } else {
        Arc::new(Tether::with_keyring)
    }
}

#[derive(Clone)]
pub struct Backend {
    app: AppHandle,
    db: Db,
    launcher: Launcher,
    events: mpsc::Sender<LaunchEvent>,
    art_v: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    pub art: Arc<ArtIndex>,
    /// Game names by id, for download items (refreshed with the art index).
    names: Arc<std::sync::RwLock<HashMap<String, String>>>,
    tether: TetherService,
    downloads: Option<grapple_download::Downloads>,
}

impl Backend {
    pub fn new(app: &AppHandle) -> Result<Backend> {
        let db = Db::open_default().context("open the Grapple database")?;
        let app_settings: Settings = db.setting(SETTINGS_KEY).ok().flatten().unwrap_or_default();
        let manual = manual_games::load(&db);
        let cached = manual_games::merge(db.games().unwrap_or_default(), &manual);
        let mut ls = LaunchSettings::default();
        apply_defaults(&app_settings, &mut ls); // keep_steam_ready: spawn_watcher starts Steam hidden when it is on
        crate::quiet::gate().set_on(app_settings.quiet_while_playing);
        let maps = launch_maps(&db, &cached, &manual, &app_settings);
        ls.close_steam_per_game = maps.close_steam_per_game;
        ls.wrappers = maps.wrappers;
        ls.proton_per_game = maps.proton;
        ls.pad_mode_per_game = maps.pad_mode;
        ls.manual = maps.manual;
        let launcher = Launcher::new(db.clone(), ls);
        launcher.set_known_games(&cached);

        let handle = app.clone();
        // Play pressed on an invite's card in tether: handled below, once the backend exists.
        let (play_tx, mut play_rx) = mpsc::unbounded_channel::<PlayRequest>();
        let tether = {
            // The service's actor task runs on tauri's tokio runtime (block_on gives the spawn a runtime context).
            let emit_to = handle.clone();
            let emit_invites_to = handle.clone();
            tauri::async_runtime::block_on(async {
                TetherService::spawn(Deps {
                    db: db.clone(),
                    make_tether: tether_factory(),
                    browser: Arc::new(crate::tether_svc::SessionBrowser),
                    emit: Arc::new(move |s| {
                        let _ = emit_to.emit("tether-changed", s);
                    }),
                    icon_dir: cache_dir().join("tether-icons"),
                    avatar_dir: cache_dir().join("avatars"),
                    desktop_settings: crate::tether_svc::desktop_settings_path(),
                    emit_invites: Arc::new(move |s| {
                        let _ = emit_invites_to.emit("invites-changed", s);
                    }),
                    play: Arc::new(move |r| {
                        let _ = play_tx.send(r);
                    }),
                })
            })
        };
        tether.set_library(&cached);
        tether.start();

        let art = Arc::new(ArtIndex::with_custom(grapple_launch::paths::data_home().join("grapple").join("art")));
        let art_v = art.update(&cached);
        let (tx, mut rx) = mpsc::channel::<LaunchEvent>(64);
        let names = Arc::new(std::sync::RwLock::new(cached.iter().map(|g| (g.id.0.clone(), g.name.clone())).collect()));
        let downloads = crate::downloads::init(app, launcher.clone());
        let me = Backend { app: app.clone(), db, launcher, events: tx.clone(), art_v: Arc::new(std::sync::RwLock::new(art_v)), art, names, tether, downloads };

        // Every LaunchEvent from the launcher (and from its /proc watcher) goes to the UI as `launch-event`,
        // and drives "now playing" in tether.
        let be = me.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match &ev {
                    LaunchEvent::Running { .. } => be.sync_now_playing(None),
                    LaunchEvent::Exited { game, .. } => {
                        be.sync_now_playing(Some(game));
                        let _ = be.app.emit("games-changed", ());
                    }
                    _ => {}
                }
                // "Nothing while I'm playing": notifications wait until the last game closes.
                if let LaunchEvent::Running { .. } | LaunchEvent::Exited { .. } = &ev {
                    let exited = if let LaunchEvent::Exited { game, .. } = &ev { Some(game) } else { None };
                    let playing = be.launcher.running().iter().any(|(g, _)| Some(g) != exited);
                    let held = crate::quiet::gate().set_playing(playing);
                    for (title, body) in held {
                        crate::store::send_desktop(title, body);
                    }
                }
                // Pause downloads while playing (and Steam's restart after its game, for the bridge switch).
                if matches!(ev, LaunchEvent::Running { .. } | LaunchEvent::Exited { .. })
                    && let Some(dl) = &be.downloads
                {
                    let running = be.launcher.running();
                    dl.set_playing(!running.is_empty(), running.iter().any(|(g, _)| g.0.starts_with("steam:"))).await;
                }
                let _ = be.app.emit("launch-event", &ev);
            }
        });
        me.launcher.spawn_watcher(tx);

        // GAME_INVITE_PLAY: Josh pressed Play in tether, so grapple acts at once (no question): launch the game if
        // it is installed here, else the install sheet or the store page. Several computers of his may all get
        // it; v1 acts on each (only the one with the game installed starts it; the others only show a sheet).
        let be = me.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(r) = play_rx.recv().await {
                let join = r.join.map(|grapple_tether::InviteJoin::Text(t)| t.expose().to_string());
                let outcome = be.act_on_invite(&r.game.id, &r.game.name, join).await;
                crate::windows::show_main(&be.app);
                let _ = be.app.emit("invite-play", &outcome);
            }
        });
        Ok(me)
    }

    /// Background rescan, called once the window is up (the UI already has the cached list).
    pub fn start(&self) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = me.rescan().await {
                tracing::warn!("rescan failed: {e}");
            }
        });
    }

    /// Close the tether connection cleanly (tether then forgets the session and the game). Called at exit.
    pub fn shutdown(&self) {
        let svc = self.tether.clone();
        tauri::async_runtime::block_on(svc.shutdown(std::time::Duration::from_secs(3)));
    }

    fn update_launch_settings(&self, f: impl FnOnce(&mut LaunchSettings)) {
        let mut s = (*self.launcher.settings()).clone();
        f(&mut s);
        self.launcher.update_settings(s);
    }

    /// Scanned games (the Db cache) plus the manual ones.
    fn all_games(&self) -> Result<Vec<Game>> {
        Ok(manual_games::merge(self.db.games()?, &manual_games::load(&self.db)))
    }

    /// Recompute the launcher's manual entries and per-game switches from the Db.
    fn sync_launch_settings(&self, games: &[Game]) {
        let settings = self.get_settings();
        let maps = launch_maps(&self.db, games, &manual_games::load(&self.db), &settings);
        self.update_launch_settings(|l| {
            apply_defaults(&settings, l);
            l.manual = maps.manual;
            l.close_steam_per_game = maps.close_steam_per_game;
            l.wrappers = maps.wrappers;
            l.proton_per_game = maps.proton;
            l.pad_mode_per_game = maps.pad_mode;
            l.eos_overlay_games = maps.eos_overlay;
            l.eos_overlay_dir = self.downloads.as_ref().and_then(|d| d.eos_overlay_installed());
        });
    }

    /// Tell tether what is playing now: the newest running game that the global and per-game switches allow.
    fn sync_now_playing(&self, exited: Option<&GameId>) {
        let games = self.all_games().unwrap_or_default();
        let settings = self.get_settings();
        let playing: Option<Playing> = choose_playing(&self.launcher.running(), &games, exited, settings.show_in_tether, |id| {
            self.get_game_settings(&id.0).show_in_tether
        });
        self.tether.set_playing(playing);
    }

    pub async fn list_games(&self) -> Result<Vec<GameDto>> {
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<Vec<GameDto>> {
            let games = me.all_games()?;
            let favs: std::collections::HashSet<String> = me.db.favourites()?.into_iter().map(|g| g.0).collect();
            let running: Vec<String> = me.launcher.running().into_iter().map(|(g, _)| g.0).collect();
            let played = me.db.last_played_by_sessions().unwrap_or_default();
            let settings = me.get_settings();
            let now = now_secs();
            let vs = me.art_v.read().unwrap();
            Ok(games
                .iter()
                .map(|g| {
                    let total = me.db.stats(&g.id, g.store_playtime_min, now).map(|s| s.total_min).unwrap_or(g.store_playtime_min);
                    let mut g = g.clone();
                    g.art = me.art.effective(&g.id.0, &g.art);
                    g.last_played = g.last_played.max(played.get(&g.id.0).copied());
                    let mut dto = GameDto::new(&g, favs.contains(&g.id.0), total, *vs.get(&g.id.0).unwrap_or(&0));
                    if running.contains(&g.id.0) {
                        dto.status = Some(GameStatus::Running);
                    }
                    dto.hidden = settings.hides(&g.id.0, g.store, g.installed);
                    dto
                })
                .collect())
        })
        .await?
    }

    fn find(&self, id: &str) -> Result<Game> {
        self.all_games()?.into_iter().find(|g| g.id.0 == id).ok_or_else(|| anyhow!("unknown game"))
    }

    pub async fn game_stats(&self, id: &str) -> Result<PlayStats> {
        let me = self.clone();
        let id = id.to_string();
        tauri::async_runtime::spawn_blocking(move || {
            let g = me.find(&id)?;
            Ok(with_week(me.db.stats(&g.id, g.store_playtime_min, now_secs())?, &me.get_settings().week_start))
        })
        .await?
    }

    pub async fn library_stats(&self) -> Result<LibraryStats> {
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let now = now_secs();
            let week = me.get_settings().week_start;
            let all: Vec<(String, PlayStats)> = me
                .all_games()?
                .iter()
                .filter_map(|g| Some((g.id.0.clone(), with_week(me.db.stats(&g.id, g.store_playtime_min, now).ok()?, &week))))
                .collect();
            Ok(LibraryStats::sum(all.iter().map(|(i, s)| (i.as_str(), s))))
        })
        .await?
    }

    pub async fn launch(&self, id: &str) -> Result<()> {
        let me = self.clone();
        let id = id.to_string();
        let game = tauri::async_runtime::spawn_blocking({
            let me = me.clone();
            let id = id.clone();
            move || me.find(&id)
        })
        .await??;
        let _ = self.app.emit("launch-started", serde_json::json!({ "game": id }));
        // Launcher::launch reports progress through `events` and sends the Failed event itself on error.
        tauri::async_runtime::spawn(async move {
            if let Err(e) = me.launcher.launch(&game, me.events.clone()).await {
                tracing::warn!("launch of {} failed: {e}", game.id.0);
            }
        });
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        self.launcher.stop(&GameId(id.to_string())).await.map_err(|e| anyhow!(e.to_string()))
    }

    pub fn running(&self) -> Vec<(String, i64)> {
        self.launcher.running().into_iter().map(|(g, t)| (g.0, t)).collect()
    }

    pub async fn set_favourite(&self, id: &str, on: bool) -> Result<()> {
        self.db.set_favourite(&GameId(id.to_string()), on)?;
        Ok(())
    }

    pub async fn rescan(&self) -> Result<()> {
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<()> {
            let res = scan_all(&ScanConfig::detect());
            for w in &res.warnings {
                tracing::info!("scan: {w}");
            }
            *me.art_v.write().unwrap() = me.art.update(&res.games);
            me.db.replace_games(&res.games)?;
            let all = me.all_games()?;
            *me.names.write().unwrap() = all.iter().map(|g| (g.id.0.clone(), g.name.clone())).collect();
            me.launcher.set_known_games(&all);
            me.tether.set_library(&all);
            me.sync_launch_settings(&all);
            let _ = me.db.set_setting(LAST_SCAN_KEY, &now_secs());
            Ok(())
        })
        .await??;
        let _ = self.app.emit("games-changed", ());
        Ok(())
    }

    pub fn get_settings(&self) -> Settings {
        self.db.setting(SETTINGS_KEY).ok().flatten().unwrap_or_default()
    }

    pub fn set_settings(&self, s: Settings) -> Result<Settings> {
        let before = self.get_settings();
        self.db.set_setting(SETTINGS_KEY, &s)?;
        // The defaults for games without their own choice (GameMode, MangoHud, Proton, close Steam).
        if let Ok(games) = self.all_games() {
            self.sync_launch_settings(&games);
        }
        let held = crate::quiet::gate().set_on(s.quiet_while_playing);
        for (title, body) in held {
            crate::store::send_desktop(title, body);
        }
        self.sync_now_playing(None); // "Show what I play in tether" may have changed
        if s.lists_differ(&before) || s.week_start != before.week_start {
            let _ = self.app.emit("games-changed", ());
        }
        Ok(s)
    }

    /// Does grapple start this game through Proton (Epic games, Windows programs added by hand; Linux)? The
    /// Controller choice of a game's settings only applies to those.
    pub fn uses_proton(&self, id: &str) -> bool {
        cfg!(target_os = "linux")
            && (id.starts_with("epic:")
                || manual_games::load(&self.db)
                    .iter()
                    .any(|m| m.id == id && m.entry.effective_kind() == grapple_launch::manual::ManualKind::Windows))
    }

    pub fn get_game_settings(&self, id: &str) -> GameSettings {
        self.db.setting(&game_settings_key(id)).ok().flatten().unwrap_or_default()
    }

    pub fn set_game_settings(&self, id: &str, s: GameSettings) {
        let _ = self.db.set_setting(&game_settings_key(id), &s);
        if let Ok(games) = self.all_games() {
            self.sync_launch_settings(&games);
        }
        self.sync_now_playing(None); // the per-game "Show in tether" switch may have changed
    }

    /// "Epic overlay" for an Epic game. The first time it is switched on, Epic's overlay is downloaded first.
    pub async fn eos_overlay_set(&self, id: &str, on: bool) -> Result<()> {
        if !id.starts_with("epic:") {
            return Err(anyhow!("Only Epic games use Epic's overlay"));
        }
        if on && self.dl()?.eos_overlay_installed().is_none() {
            self.dl()?.eos_overlay_install().await.map_err(|e| anyhow!(e.short()))?;
        }
        let mut gs = self.get_game_settings(id);
        gs.eos_overlay = on;
        self.set_game_settings(id, gs);
        Ok(())
    }

    // ------------------------------------------------------------------------------- art

    /// Which kinds of this game's art the user picked (`"cover" | "hero" | "logo" | "icon"`).
    pub fn art_custom(&self, id: &str) -> Vec<&'static str> {
        ArtKind::ALL.into_iter().filter(|k| self.art.custom_file(id, *k).is_some()).map(ArtKind::name).collect()
    }

    /// Pick a picture (a file picker) for one kind of a game's art. `false`: cancelled.
    pub async fn art_pick(&self, id: &str, kind: &str) -> Result<bool> {
        use tauri_plugin_dialog::DialogExt;
        let kind = ArtKind::parse(kind).ok_or_else(|| anyhow!("unknown kind of art"))?;
        let dir = self.art.custom_dir(id).ok_or_else(|| anyhow!("not a game"))?;
        let app = self.app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || {
            app.dialog().file().set_title("Choose a picture").add_filter("Pictures", &["png", "jpg", "jpeg", "webp"]).blocking_pick_file()
        })
        .await?;
        let Some(file) = picked else { return Ok(false) };
        let src = file.into_path().map_err(|e| anyhow!("{e}"))?;
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<()> {
            super::save_custom_art(&src, &dir, kind)?;
            me.refresh_art()
        })
        .await??;
        let _ = self.app.emit("games-changed", ());
        Ok(true)
    }

    /// Back to the store's own art for one kind.
    pub async fn art_reset(&self, id: &str, kind: &str) -> Result<()> {
        let kind = ArtKind::parse(kind).ok_or_else(|| anyhow!("unknown kind of art"))?;
        let dir = self.art.custom_dir(id).ok_or_else(|| anyhow!("not a game"))?;
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<()> {
            super::remove_custom_art(&dir, kind);
            me.refresh_art()
        })
        .await??;
        let _ = self.app.emit("games-changed", ());
        Ok(())
    }

    /// Re-index the art (a picture was picked or reset), so URLs carry a new `?v=`.
    fn refresh_art(&self) -> Result<()> {
        let games = self.all_games()?;
        *self.art_v.write().unwrap() = self.art.update(&games);
        Ok(())
    }

    // ------------------------------------------------------------------------------- downloads

    fn dl(&self) -> Result<&grapple_download::Downloads> {
        self.downloads.as_ref().ok_or_else(|| anyhow!("Downloads are not available"))
    }

    /// A game's name and art for a download item.
    pub fn dl_game(&self, id: &str) -> Option<grapple_download::GameInfo> {
        let name = self.names.read().unwrap().get(id).cloned()?;
        let v = self.art_v.read().unwrap().get(id).copied().unwrap_or(0);
        let enc = id.replace(':', "%3A");
        let url = |kind: &str| {
            if cfg!(windows) { format!("http://grapple.localhost/art/{enc}/{kind}?v={v}") } else { format!("grapple://art/{enc}/{kind}?v={v}") }
        };
        let has = |k: ArtKind| self.art.path(id, k).is_some_and(|p| p.exists());
        Some(grapple_download::GameInfo {
            name,
            art: grapple_download::DlArt { hero: has(ArtKind::Hero).then(|| url("hero")), header: None, cover: has(ArtKind::Cover).then(|| url("cover")) },
        })
    }

    pub async fn downloads_snapshot(&self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(self.dl()?.snapshot_on_demand())?)
    }

    pub async fn install_plan(&self, id: &str) -> Result<serde_json::Value> {
        let plan = self.dl()?.install_plan(id).await.map_err(|e| anyhow!(e.short()))?;
        Ok(serde_json::to_value(plan)?)
    }

    /// Queue an install. The "Runs with" choice is kept in grapple's per-game settings (Epic, Linux).
    pub async fn install_start(&self, id: &str, target_dir: &str, proton: Option<String>) -> Result<()> {
        if cfg!(target_os = "linux")
            && id.starts_with("epic:")
            && let Some(p) = proton.filter(|p| !p.trim().is_empty())
        {
            let mut gs = self.get_game_settings(id);
            gs.proton = Some(p);
            self.set_game_settings(id, gs);
        }
        self.dl()?.install_start(id, target_dir).await.map_err(|e| anyhow!(e.short()))
    }

    /// The old Install button: `install_start` into the default drive.
    pub async fn install(&self, id: &str) -> Result<()> {
        let plan = self.dl()?.install_plan(id).await.map_err(|e| anyhow!(e.short()))?;
        let target = plan.targets.iter().find(|t| t.is_default).or(plan.targets.first()).ok_or_else(|| anyhow!("No drive to install to"))?;
        self.install_start(id, &target.path.clone(), None).await
    }

    pub async fn download_action(&self, what: &str, id: &str) -> Result<()> {
        let dl = self.dl()?;
        let r = match what {
            "pause" => dl.pause(id).await,
            "resume" => dl.resume(id).await,
            "cancel" => dl.cancel(id).await,
            "start_now" => dl.start_now(id).await,
            "update_now" => dl.update_now(id).await,
            "uninstall" => dl.uninstall(id).await,
            _ => return Err(anyhow!("unknown action")),
        };
        r.map_err(|e| anyhow!(e.short()))
    }

    /// "Check for update": `"upToDate" | "available" | "queued" | "downloading"`.
    pub async fn check_update(&self, id: &str) -> Result<serde_json::Value> {
        let r = self.dl()?.check_update(id).await.map_err(|e| anyhow!(e.short()))?;
        Ok(serde_json::to_value(r)?)
    }

    pub async fn download_reorder(&self, ids: Vec<String>) -> Result<()> {
        self.dl()?.reorder(&ids).await.map_err(|e| anyhow!(e.short()))
    }

    pub async fn downloads_pause_all(&self, paused: bool) -> Result<()> {
        self.dl()?.pause_all(paused).await.map_err(|e| anyhow!(e.short()))
    }

    pub async fn download_settings_get(&self) -> Result<serde_json::Value> {
        let dl = self.dl()?.clone();
        let s = tauri::async_runtime::spawn_blocking(move || dl.settings_get()).await?;
        Ok(serde_json::to_value(s)?)
    }

    pub async fn download_settings_set(&self, v: serde_json::Value) -> Result<serde_json::Value> {
        let s: grapple_download::DlSettings = serde_json::from_value(v)?;
        let dl = self.dl()?.clone();
        let s = tauri::async_runtime::spawn_blocking(move || dl.settings_set(s)).await?.map_err(|e| anyhow!(e.short()))?;
        Ok(serde_json::to_value(s)?)
    }

    pub async fn steam_bridge_set(&self, on: bool) -> Result<serde_json::Value> {
        let s = self.dl()?.steam_bridge_set(on).await.map_err(|e| anyhow!(e.short()))?;
        Ok(serde_json::to_value(s)?)
    }

    // ------------------------------------------------------------------------------- manual games

    /// The file picker (xdg-desktop-portal or GTK through the dialog plugin). `None`: cancelled.
    pub async fn pick_program(&self) -> Result<Option<serde_json::Value>> {
        use tauri_plugin_dialog::DialogExt;
        let app = self.app.clone();
        let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_title("Choose the game's program").blocking_pick_file())
            .await?;
        let Some(file) = picked else { return Ok(None) };
        let path = file.into_path().map_err(|e| anyhow!("{e}"))?;
        Ok(Some(serde_json::to_value(manual_games::detect(&path.to_string_lossy()))?))
    }

    pub fn detect_program(&self, path: &str) -> Detected {
        manual_games::detect(path)
    }

    pub async fn add_manual_game(&self, draft: ManualDraft) -> Result<String> {
        let mut m = manual_games::build(&draft, manual_games::new_id(), now_secs()).map_err(|e| anyhow!(e))?;
        if m.entry.effective_kind() != grapple_launch::manual::ManualKind::Command && !std::path::Path::new(&m.entry.exe).is_file() {
            return Err(anyhow!("That file does not exist"));
        }
        m.name = m.name.trim().to_string();
        let id = m.id.clone();
        let mut list = manual_games::load_list(&self.db);
        manual_games::add(&mut list.games, m);
        manual_games::save_list(&self.db, &list)?;
        self.manual_changed();
        Ok(id)
    }

    pub async fn remove_manual_game(&self, id: &str) -> Result<()> {
        let mut list = manual_games::load_list(&self.db);
        if !manual_games::remove(&mut list.games, id) {
            return Err(anyhow!("that game was not added by hand"));
        }
        manual_games::save_list(&self.db, &list)?;
        let gid = GameId(id.to_string());
        let _ = self.db.remove_game(&gid);
        let _ = self.db.set_favourite(&gid, false);
        self.manual_changed();
        Ok(())
    }

    fn manual_changed(&self) {
        if let Ok(games) = self.all_games() {
            *self.names.write().unwrap() = games.iter().map(|g| (g.id.0.clone(), g.name.clone())).collect();
            self.launcher.set_known_games(&games);
            self.tether.set_library(&games);
            self.sync_launch_settings(&games);
        }
        let _ = self.app.emit("games-changed", ());
    }

    // ------------------------------------------------------------------------------- the settings page

    /// Settings › Library: sources with counts, the folders read (with free space), the last scan, hidden games.
    pub async fn library_info(&self) -> Result<LibraryInfo> {
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let games = me.all_games()?;
            let steam_libs = grapple_scan::ScanConfig::detect().steam_root.as_deref().map(grapple_scan::steam_library_dirs).unwrap_or_default();
            let home = grapple_launch::paths::home();
            let mut folders = watched_folders(&games, &steam_libs, &home);
            for f in &mut folders {
                let p = grapple_download::fsutil::expand_home(&f.path, &home);
                f.free_bytes = grapple_download::fsutil::free_space(&p).ok().map(|s| s.free);
            }
            Ok(LibraryInfo {
                sources: SourceDto::count(&games),
                folders,
                last_scan: me.db.setting(LAST_SCAN_KEY).ok().flatten(),
                hidden: hidden_named(&me.get_settings(), &games),
            })
        })
        .await?
    }

    /// Settings › Playing (Linux): every Proton found, where it comes from, and what "Runs with" offers.
    pub async fn protons(&self) -> Result<ProtonsDto> {
        if !cfg!(target_os = "linux") {
            return Ok(ProtonsDto::default());
        }
        Ok(tauri::async_runtime::spawn_blocking(|| {
            let (list, heroic_default) = crate::downloads::proton_list();
            let mut choices: Vec<String> = Vec::new();
            if grapple_launch::proton::latest_ge(&list).is_some() {
                choices.push(grapple_launch::proton::LATEST_GE.to_string());
            }
            choices.extend(list.iter().map(|p| p.name.clone()));
            let versions = list.iter().map(|p| ProtonDto { name: p.name.clone(), source: proton_source(&p.bin).into() }).collect();
            ProtonsDto { choices, versions, heroic_default }
        })
        .await?)
    }

    /// Settings › Accounts. Steam's profile name from `loginusers.vdf` (never the account name); Epic: whether
    /// legendary's login file exists, and the display name through grapple-launch (no token is read here).
    pub async fn accounts(&self) -> Result<AccountsDto> {
        Ok(tauri::async_runtime::spawn_blocking(|| {
            let steam_root = grapple_scan::ScanConfig::detect().steam_root;
            let steam_name = steam_root
                .as_ref()
                .and_then(|r| std::fs::read_to_string(r.join("config").join("loginusers.vdf")).ok())
                .and_then(|t| steam_persona(&t));
            let user_json = crate::downloads::places().legendary_dir.join("user.json");
            let epic_signed_in = user_json.is_file();
            let epic_name = epic_signed_in
                .then(|| grapple_launch::epic_auth::EpicAuth::new(grapple_launch::epic_auth::EpicApi::default(), user_json).ok()?.identity().ok())
                .flatten()
                .map(|i| i.display_name);
            AccountsDto { steam_found: steam_root.is_some(), steam_name, store_data: crate::store_window::site_data().is_some(), epic_signed_in, epic_name }
        })
        .await?)
    }

    /// "Sign out of store": the store window's cookies go (the Steam app and the Epic login stay).
    pub async fn store_sign_out(&self) -> Result<()> {
        crate::store_window::sign_out(&self.app).await.map_err(|e| anyhow!(e))
    }

    /// The Files tab: the game's folder and its size (the store's number, else counted).
    pub async fn game_files(&self, id: &str) -> Result<GameFilesDto> {
        let g = self.find(id)?;
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let dir = g.install_dir.filter(|d| g.installed && d.is_dir());
            let size_bytes = g.size_bytes.filter(|b| *b > 0).or_else(|| dir.as_deref().map(crate::system::dir_size));
            GameFilesDto { folder: dir.as_deref().map(crate::system::tilde), size_bytes }
        })
        .await?)
    }

    /// Games whose "Show it to friends when I play" is off (Settings › Friends & tether › Games I don't share).
    pub async fn games_not_shared(&self) -> Result<Vec<String>> {
        let me = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            Ok(me.all_games()?.into_iter().map(|g| g.id.0).filter(|id| !me.get_game_settings(id).show_in_tether).collect())
        })
        .await?
    }

    /// Settings › Advanced › Storage.
    pub async fn storage(&self) -> Result<StorageDto> {
        Ok(tauri::async_runtime::spawn_blocking(|| {
            let (cache, data, logs) = (crate::system::cache_dir(), crate::system::data_dir(), grapple_launch::paths::log_dir());
            StorageDto {
                cache_bytes: crate::system::dir_size(&cache),
                data_bytes: crate::system::dir_size(&data),
                has_log: crate::system::newest_file(&logs).is_some(),
                cache_path: crate::system::tilde(&cache),
                data_path: crate::system::tilde(&data),
                logs_path: crate::system::tilde(&logs),
            }
        })
        .await?)
    }

    /// Clear `$XDG_CACHE_HOME/grapple` (profile pictures, tether icons, store pictures): all fetched again when
    /// needed. Returns the bytes freed.
    pub async fn cache_clear(&self) -> Result<u64> {
        Ok(tauri::async_runtime::spawn_blocking(|| crate::system::clear_dir(&crate::system::cache_dir())).await??)
    }

    /// Open one of grapple's folders (never a path from the page): `data`, `logs`, or a game's folder.
    pub async fn open_place(&self, place: &str, game: Option<&str>) -> Result<()> {
        let path = match (place, game) {
            ("data", _) => crate::system::data_dir(),
            ("logs", _) => grapple_launch::paths::log_dir(),
            ("game", Some(id)) => self.find(id)?.install_dir.filter(|d| d.is_dir()).ok_or_else(|| anyhow!("The game's folder is not there"))?,
            _ => return Err(anyhow!("unknown place")),
        };
        if place != "game" {
            std::fs::create_dir_all(&path)?;
        }
        crate::system::open(&path)?;
        Ok(())
    }

    /// The newest game log (the last launch), its last 64 KB.
    pub async fn last_launch_log(&self) -> Result<String> {
        Ok(tauri::async_runtime::spawn_blocking(|| -> Result<String> {
            let file = crate::system::newest_file(&grapple_launch::paths::log_dir()).ok_or_else(|| anyhow!("No game log yet"))?;
            Ok(crate::system::redact_secrets(&crate::system::tail(&file, 64 * 1024)?))
        })
        .await??)
    }

    /// "Reset settings": grapple's own settings back to their defaults. Playtime, favourites, per-game choices
    /// and sign-ins are kept.
    pub fn settings_reset(&self) -> Result<Settings> {
        self.set_settings(Settings::default())
    }

    // ------------------------------------------------------------------------------- tether

    /// The cached picture of a person in the current tether snapshot (the `grapple://avatar/<id>` protocol).
    pub fn avatar(&self, user_id: &str) -> Option<AvatarFile> {
        self.tether.avatar(user_id)
    }

    pub fn tether_status(&self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(self.tether.snapshot())?)
    }

    pub async fn tether_sign_in(&self) -> Result<serde_json::Value> {
        let s = self.tether.sign_in().await.map_err(|e| anyhow!(e))?;
        Ok(serde_json::to_value(s)?)
    }

    pub fn tether_cancel_sign_in(&self) {
        self.tether.cancel_sign_in();
    }

    pub async fn tether_sign_out(&self) -> Result<serde_json::Value> {
        let s = self.tether.sign_out().await.map_err(|e| anyhow!(e))?;
        Ok(serde_json::to_value(s)?)
    }

    pub async fn tether_set_instance_url(&self, url: String) -> Result<serde_json::Value> {
        let s = self.tether.set_instance_url(url).await.map_err(|e| anyhow!(e))?;
        Ok(serde_json::to_value(s)?)
    }

    // ------------------------------------------------------------------------------- invites

    pub fn invites_snapshot(&self) -> InvitesSnapshot {
        self.tether.invites_snapshot()
    }

    /// Invite these friends to a library game (one invite each, open for 15 minutes).
    pub async fn invite_send(&self, game: &str, friends: Vec<String>) -> Result<Vec<SendResult>> {
        let g = self.find(game)?;
        self.tether.send_invites(&g.id.0, &g.name, art_source(&g), &friends).await.map_err(|e| anyhow!(e))
    }

    pub async fn invite_cancel(&self, id: &str) -> Result<()> {
        self.tether.cancel_invite(id).await.map_err(|e| anyhow!(e))
    }

    pub async fn invite_decline(&self, id: &str) -> Result<()> {
        self.tether.decline_invite(id).await.map_err(|e| anyhow!(e))
    }

    pub async fn invites_seen(&self, ids: Vec<String>) -> Result<()> {
        self.tether.invites_seen(&ids).await.map_err(|e| anyhow!(e))
    }

    /// Play on an invite in grapple (the person pressed it): accept it, then act like Play in tether does.
    pub async fn invite_accept(&self, id: &str) -> Result<PlayOutcome> {
        let (game, join) = self.tether.accept_invite(id).await.map_err(|e| anyhow!(e))?;
        let join = join.map(|grapple_tether::InviteJoin::Text(t)| t.expose().to_string());
        Ok(self.act_on_invite(&game.id, &game.name, join).await)
    }

    /// What Play means here (`crate::invites::decide`, from the library as it is now). An installed game is
    /// launched through the normal launch path; everything else is for the UI to open.
    async fn act_on_invite(&self, game_id: &str, name: &str, join: Option<String>) -> PlayOutcome {
        let library: Vec<LibGame> = self.all_games().unwrap_or_default().iter().map(LibGame::of).collect();
        let action = decide(game_id, name, &library);
        let mut error = None;
        if let PlayAction::Launch { game } = &action
            && let Err(e) = self.launch(game).await
        {
            error = Some(e.to_string());
        }
        PlayOutcome { action, name: name.to_string(), join, error }
    }

    /// `grapple://invite/<id>` (checked by the caller): show that invite with one Play button. Never plays it.
    pub fn open_invite(&self, id: String) {
        self.tether.open_invite(id);
    }

    pub fn invite_close(&self) {
        self.tether.close_invite();
    }

    pub async fn invite_settings_get(&self) -> Result<String> {
        let from = self.tether.invite_settings().await.map_err(|e| anyhow!(e))?;
        Ok(invites_from_name(from).into())
    }

    pub async fn invite_settings_set(&self, from: &str) -> Result<String> {
        let from = match from {
            "friends" => grapple_tether::InvitesFrom::Friends,
            "nobody" => grapple_tether::InvitesFrom::Nobody,
            _ => return Err(anyhow!("unknown choice")),
        };
        let from = self.tether.set_invite_settings(from).await.map_err(|e| anyhow!(e))?;
        Ok(invites_from_name(from).into())
    }
}

fn invites_from_name(from: grapple_tether::InvitesFrom) -> &'static str {
    match from {
        grapple_tether::InvitesFrom::Friends => "friends",
        grapple_tether::InvitesFrom::Nobody => "nobody",
    }
}

/// Where a Proton comes from, by its folder: Heroic's tools, Steam's compatibility tools, or Steam itself.
fn proton_source(bin: &std::path::Path) -> &'static str {
    let p = bin.to_string_lossy();
    if p.contains("/heroic/") {
        "heroic"
    } else if p.contains("compatibilitytools.d") {
        "steam-tools"
    } else {
        "steam"
    }
}

/// The profile name (PersonaName) of the most recent Steam login in `loginusers.vdf`; the first user when none
/// is marked. The account (login) name is never read.
pub fn steam_persona(text: &str) -> Option<String> {
    let vdf = grapple_scan::vdf::Vdf::parse(text);
    let users = vdf.get("users")?.entries();
    let recent = users.iter().find(|(_, u)| u.str_at(&["MostRecent"]) == Some("1")).or(users.first())?;
    recent.1.str_at(&["PersonaName"]).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string)
}

/// The folders grapple reads games from: every Steam library (with its installed Steam games) and the folders
/// installed Epic games sit in. Free space is filled in by the caller.
pub fn watched_folders(games: &[Game], steam_libs: &[std::path::PathBuf], home: &std::path::Path) -> Vec<FolderDto> {
    let installed_in = |store: Store, dir: &std::path::Path| {
        games.iter().filter(|g| g.store == store && g.installed && g.install_dir.as_deref().is_some_and(|d| d.starts_with(dir))).count()
    };
    let mut out: Vec<FolderDto> = steam_libs
        .iter()
        .map(|lib| FolderDto { path: grapple_download::fsutil::tilde(lib, home), store: "steam".into(), installed: installed_in(Store::Steam, lib), free_bytes: None })
        .collect();
    let mut epic: Vec<std::path::PathBuf> = games
        .iter()
        .filter(|g| g.store == Store::Epic && g.installed)
        .filter_map(|g| g.install_dir.as_deref()?.parent().map(std::path::Path::to_path_buf))
        .collect();
    epic.sort();
    epic.dedup();
    for dir in epic {
        let n = games.iter().filter(|g| g.store == Store::Epic && g.installed && g.install_dir.as_deref().and_then(std::path::Path::parent) == Some(dir.as_path())).count();
        out.push(FolderDto { path: grapple_download::fsutil::tilde(&dir, home), store: "epic".into(), installed: n, free_bytes: None });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual_games::NewManual;
    use grapple_core::sort_name;

    fn game(store: Store, id: &str) -> Game {
        Game {
            id: GameId::new(store, id),
            store,
            name: id.into(),
            sort_name: sort_name(id),
            installed: true,
            install_dir: None,
            size_bytes: None,
            art: Default::default(),
            store_playtime_min: 0,
            last_played: None,
            status: None,
        }
    }

    #[test]
    fn launch_maps_split_steam_from_the_games_grapple_starts() {
        let db = Db::open_in_memory().unwrap();
        let games = vec![game(Store::Steam, "730"), game(Store::Steam, "1"), game(Store::Epic, "Sugar"), game(Store::Manual, "m")];
        let manual = vec![
            manual_games::build(&NewManual { name: "M".into(), path: "/p/game".into(), kind: "native".into(), args: String::new() }, "manual:m".into(), 0).unwrap(),
        ];
        db.set_setting(&game_settings_key("steam:730"), &GameSettings { close_steam_on_quit: Some(false), ..GameSettings::default() }).unwrap();
        db.set_setting(&game_settings_key("steam:1"), &GameSettings { show_in_tether: false, ..GameSettings::default() }).unwrap();
        db.set_setting(&game_settings_key("epic:Sugar"), &GameSettings { gamemode: Some(false), mangohud: Some(true), ..GameSettings::default() }).unwrap();

        let m = launch_maps(&db, &games, &manual, &Settings::default());
        // Steam: only the explicit "close Steam" choice; never wrappers (Steam starts the process)
        assert_eq!(m.close_steam_per_game.len(), 1);
        assert_eq!(m.close_steam_per_game[&GameId("steam:730".into())], false);
        assert!(!m.wrappers.contains_key(&GameId("steam:730".into())));
        // Epic: the saved switches; manual: the defaults (GameMode on, MangoHud off)
        assert_eq!(m.wrappers[&GameId("epic:Sugar".into())], Wrappers { gamemode: false, mangohud: true });
        assert_eq!(m.wrappers[&GameId("manual:m".into())], Wrappers { gamemode: true, mangohud: false });
        assert_eq!(m.manual[&GameId("manual:m".into())].exe, "/p/game");

        // Games without their own choice follow the defaults in the settings; a Steam game whose saved settings
        // say nothing about closing Steam gets no override (the global "Close Steam" applies).
        let defaults = Settings { default_gamemode: false, default_mangohud: true, ..Settings::default() };
        let m = launch_maps(&db, &games, &manual, &defaults);
        assert_eq!(m.wrappers[&GameId("manual:m".into())], Wrappers { gamemode: false, mangohud: true });
        assert_eq!(m.wrappers[&GameId("epic:Sugar".into())], Wrappers { gamemode: false, mangohud: true });
        assert!(!m.close_steam_per_game.contains_key(&GameId("steam:1".into())));
    }

    #[test]
    fn global_close_steam_and_defaults_reach_the_launcher() {
        let mut l = LaunchSettings::default();
        apply_defaults(&Settings { close_steam: false, keep_steam_ready: true, ..Settings::default() }, &mut l);
        assert_eq!(l.close_steam, grapple_launch::CloseSteam::Never);
        assert!(l.keep_steam_ready);
        apply_defaults(&Settings::default(), &mut l);
        assert_eq!(l.close_steam, grapple_launch::CloseSteam::WhenGrappleStartedIt);
        assert!(l.default_proton.is_none());
    }

    #[test]
    fn steam_profile_name_not_the_login_name() {
        let vdf = r#""users"
{
	"76561190000000001"
	{
		"AccountName"		"old_login"
		"PersonaName"		"Old"
		"MostRecent"		"0"
	}
	"76561190000000002"
	{
		"AccountName"		"secret_login"
		"PersonaName"		"player1"
		"MostRecent"		"1"
	}
}
"#;
        assert_eq!(steam_persona(vdf).as_deref(), Some("player1"));
        let one = "\"users\" { \"1\" { \"AccountName\" \"x\" \"PersonaName\" \"Only\" } }";
        assert_eq!(steam_persona(one).as_deref(), Some("Only"));
        assert_eq!(steam_persona("\"users\" { }"), None);
        assert_eq!(steam_persona("garbage"), None);
    }

    #[test]
    fn watched_folders_count_installed_games() {
        let home = std::path::Path::new("/home/j");
        let mut games = vec![game(Store::Steam, "1"), game(Store::Steam, "2"), game(Store::Steam, "3"), game(Store::Epic, "A"), game(Store::Epic, "B")];
        games[0].install_dir = Some("/home/j/.local/share/Steam/steamapps/common/One".into());
        games[1].install_dir = Some("/data/SteamLibrary/steamapps/common/Two".into());
        games[2].install_dir = Some("/data/SteamLibrary/steamapps/common/Three".into());
        games[2].installed = false;
        games[3].install_dir = Some("/home/j/Games/Heroic/A".into());
        games[4].install_dir = Some("/home/j/Games/Heroic/B".into());
        let libs = vec![std::path::PathBuf::from("/home/j/.local/share/Steam"), std::path::PathBuf::from("/data/SteamLibrary")];
        let f = watched_folders(&games, &libs, home);
        let short: Vec<(&str, &str, usize)> = f.iter().map(|x| (x.path.as_str(), x.store.as_str(), x.installed)).collect();
        assert_eq!(short, [("~/.local/share/Steam", "steam", 1), ("/data/SteamLibrary", "steam", 1), ("~/Games/Heroic", "epic", 2)]);
        assert_eq!(SourceDto::count(&games)[0], SourceDto { store: "steam".into(), games: 3, installed: 2 });
        let s = Settings { hidden_games: vec!["epic:B".into(), "steam:gone".into()], ..Settings::default() };
        assert_eq!(hidden_named(&s, &games), [NamedGame { id: "epic:B".into(), name: "B".into() }]);
    }
}
