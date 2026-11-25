//! The downloads service the app holds: one queue for Steam and Epic, its background tasks, and the operations
//! behind the Tauri commands (docs/DOWNLOADS_PLAN.md "The queue" and "Tauri commands").
//!
//! Background tasks (all on the caller's tokio runtime):
//! * runner: starts the next Epic download (one at a time) and records how it ended;
//! * ticker: samples speed while something moves and marks the snapshot dirty;
//! * emitter: pushes `downloads-changed` through [`Host::emit`], at most every `emit_every` (4 a second);
//! * steam: the file watcher (every 2 s while busy, else 30 s) merged with the bridge's live numbers;
//! * updates: Epic update checks (every 6 h, and when the Downloads page opens after 10 minutes).

use crate::engine::{self, Build, Control, EngineConfig, Job, Kind, Outcome, Phase, Progress};
use crate::epic::{EpicClient, TokenSource};
use crate::error::{Error, Result};
use crate::fsutil::{self, tilde};
use crate::legendary;
use crate::queue::{AVAILABLE, QueueDb, Row};
use crate::ratelimit::RateLimit;
use crate::steam_bridge::{self, BridgeEvent, Call, Overview, SteamBridge};
use crate::steam_watch::{SteamApp, SteamKind, SteamPhase, SteamWatch};
use crate::types::*;
use futures_util::future::BoxFuture;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const HISTORY: usize = 20;

/// What the app knows about a game.
#[derive(Debug, Clone, Default)]
pub struct GameInfo {
    pub name: String,
    pub art: DlArt,
}

/// What the service needs from the app.
pub trait Host: Send + Sync + 'static {
    /// Push a snapshot to the UI (`downloads-changed`).
    fn emit(&self, snap: &DlSnapshot);
    /// Name and art of a game in the library (`None`: not a game grapple shows, e.g. a Steam tool).
    fn game(&self, id: &str) -> Option<GameInfo>;
    /// An install or uninstall finished: rescan, then `games-changed`.
    fn games_changed(&self);
    /// "<Game> is ready".
    fn notify_ready(&self, name: &str);
    /// A Steam game runs now (Steam must not restart).
    fn steam_game_running(&self) -> bool;
    /// Hand a `steam://` URL to Steam (started the way grapple-launch starts Steam).
    fn steam_open_url(&self, url: &str) -> Result<()>;
    /// `steam -shutdown`, wait for it to exit, start it hidden again. Nothing when Steam is not running.
    fn restart_steam(&self) -> BoxFuture<'static, Result<()>>;
    /// The Proton versions for "Runs with" (Linux; empty elsewhere).
    fn protons(&self) -> Vec<ProtonChoice>;
    /// Start every Steam grapple starts from now on with `-devtools-port <port>` (`None`: Steam's default).
    fn set_steam_devtools_port(&self, _port: Option<u16>) {}
}

pub struct Config {
    pub db_path: PathBuf,
    pub legendary_dir: PathBuf,
    pub heroic_dir: PathBuf,
    pub steam_root: Option<PathBuf>,
    pub home: PathBuf,
    pub launcher_base: String,
    pub bridge_port: u16,
    pub engine: EngineConfig,
    pub tokens: Arc<dyn TokenSource>,
    pub update_every: Duration,
    pub first_update_check: Duration,
    pub steam_poll_busy: Duration,
    pub steam_poll_idle: Duration,
    pub emit_every: Duration,
    pub bridge_install_wait: Duration,
    /// Read Steam's content_log.txt from the start (tests) instead of from its end.
    pub steam_log_from_start: bool,
}

impl Config {
    pub fn new(db_path: PathBuf, legendary_dir: PathBuf, heroic_dir: PathBuf, steam_root: Option<PathBuf>, home: PathBuf, tokens: Arc<dyn TokenSource>) -> Result<Config> {
        let epic = EpicClient::new(crate::epic::LAUNCHER_BASE)?;
        Ok(Config {
            db_path,
            legendary_dir,
            heroic_dir,
            steam_root,
            home,
            launcher_base: crate::epic::LAUNCHER_BASE.into(),
            bridge_port: steam_bridge::DEFAULT_PORT,
            engine: EngineConfig::new(epic.http().clone()),
            tokens,
            update_every: Duration::from_secs(6 * 3600),
            first_update_check: Duration::from_secs(30),
            steam_poll_busy: Duration::from_secs(2),
            steam_poll_idle: Duration::from_secs(30),
            emit_every: Duration::from_millis(250),
            bridge_install_wait: Duration::from_secs(8),
            steam_log_from_start: false,
        })
    }
}

struct Running {
    id: String,
    control: Arc<Control>,
    progress: Arc<Progress>,
    /// Paused to make room (Start now, a game started): back to "queued", not "paused".
    requeue: AtomicBool,
}

#[derive(Default)]
struct Speed {
    last: Option<(u64, Instant)>,
    bps: Option<u64>,
    history: VecDeque<u64>,
}

#[derive(Default)]
struct SteamView {
    apps: HashMap<u32, SteamApp>,
    overview: Option<Overview>,
}

struct Inner {
    cfg: Config,
    host: Arc<dyn Host>,
    db: QueueDb,
    epic: EpicClient,
    limit: Arc<RateLimit>,
    settings: RwLock<DlSettings>,
    running: Mutex<Option<Arc<Running>>>,
    speeds: Mutex<HashMap<String, Speed>>,
    steam: Mutex<SteamView>,
    bridge: Mutex<Option<SteamBridge>>,
    restart_pending: AtomicBool,
    playing: AtomicBool,
    paused_steam_for_play: AtomicBool,
    paused_all: AtomicBool,
    wake: Notify,
    dirty: Notify,
    steam_wake: Notify,
    plans: Mutex<HashMap<String, (Instant, Build)>>,
    /// One overlay install or update at a time.
    overlay: tokio::sync::Mutex<()>,
    last_update_check: Mutex<Option<Instant>>,
}

#[derive(Clone)]
pub struct Downloads {
    inner: Arc<Inner>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn epic_app(id: &str) -> Option<&str> {
    id.strip_prefix("epic:").filter(|a| legendary::valid_app_name(a))
}

fn steam_app(id: &str) -> Option<u32> {
    id.strip_prefix("steam:").and_then(|a| a.parse().ok()).filter(|a| *a > 0)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> { tokio::task::spawn_blocking(f).await.map_err(|e| Error::Invalid(e.to_string()))? }

const SETTINGS_KEY: &str = "settings";
const PAUSED_ALL_KEY: &str = "paused_all";

impl Downloads {
    pub fn new(cfg: Config, host: Arc<dyn Host>) -> Result<Downloads> {
        let db = QueueDb::open(&cfg.db_path)?;
        let epic = EpicClient::new(cfg.launcher_base.clone())?;
        let mut settings: DlSettings = db.setting(SETTINGS_KEY)?.unwrap_or_default();
        if settings.epic_default_dir.trim().is_empty() {
            let d = legendary::heroic_default_install_path(&cfg.heroic_dir).unwrap_or_else(|| cfg.home.join("Games"));
            settings.epic_default_dir = tilde(&d, &cfg.home);
        }
        settings.connections = settings.connections.clamp(1, 32);
        settings.choices = None;
        let limit = Arc::new(RateLimit::new(settings.speed_limit_bytes));
        let paused_all = db.setting::<bool>(PAUSED_ALL_KEY)?.unwrap_or(false);
        // A download that was running when grapple stopped (crash, reboot) goes back in the queue and resumes from
        // its journal.
        for r in db.list()? {
            if r.store == DlStore::Epic && matches!(r.state.as_str(), "starting" | "downloading" | "verifying" | "installing") {
                db.set_state(&r.id, "queued", None)?;
            }
        }
        db.prune_done(now())?;
        Ok(Downloads {
            inner: Arc::new(Inner {
                cfg,
                host,
                db,
                epic,
                limit,
                settings: RwLock::new(settings),
                running: Mutex::new(None),
                speeds: Mutex::new(HashMap::new()),
                steam: Mutex::new(SteamView::default()),
                bridge: Mutex::new(None),
                restart_pending: AtomicBool::new(false),
                playing: AtomicBool::new(false),
                paused_steam_for_play: AtomicBool::new(false),
                paused_all: AtomicBool::new(paused_all),
                wake: Notify::new(),
                dirty: Notify::new(),
                steam_wake: Notify::new(),
                plans: Mutex::new(HashMap::new()),
                overlay: tokio::sync::Mutex::new(()),
                last_update_check: Mutex::new(None),
            }),
        })
    }

    /// Start the background tasks. Call once, inside a tokio runtime.
    pub fn start(&self) {
        let me = self.clone();
        tokio::spawn(async move { me.runner().await });
        let me = self.clone();
        tokio::spawn(async move { me.emitter().await });
        let me = self.clone();
        tokio::spawn(async move { me.ticker().await });
        let me = self.clone();
        tokio::spawn(async move { me.steam_loop().await });
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(me.inner.cfg.first_update_check).await;
            loop {
                if let Err(e) = me.check_updates().await {
                    tracing::info!("downloads: update check: {e}");
                }
                // Keep Epic's overlay current once it is installed.
                if me.eos_overlay_installed().is_some()
                    && let Err(e) = me.eos_overlay_install().await
                {
                    tracing::info!("downloads: Epic overlay update: {e}");
                }
                tokio::time::sleep(me.inner.cfg.update_every).await;
            }
        });
        let mut s = self.settings();
        // Turned on before the bridge had a port of its own: pick one now and restart Steam once onto it.
        let picked = s.steam_bridge && s.steam_bridge_port == 0;
        if picked {
            match steam_bridge::free_loopback_port() {
                Ok(p) => {
                    s.steam_bridge_port = p;
                    if let Err(e) = self.inner.db.set_setting(SETTINGS_KEY, &s) {
                        tracing::warn!("downloads: saving the bridge port: {e}");
                    }
                    *self.inner.settings.write().unwrap() = s.clone();
                }
                Err(e) => tracing::warn!("downloads: {e}"),
            }
        }
        if s.steam_bridge && s.steam_bridge_port != 0 {
            self.inner.host.set_steam_devtools_port(Some(s.steam_bridge_port));
        }
        if s.steam_bridge {
            self.spawn_bridge();
        }
        if picked && s.steam_bridge_port != 0 {
            self.restart_steam_when_free();
        }
    }

    /// Restart Steam now, or after the Steam game that runs.
    fn restart_steam_when_free(&self) {
        if self.inner.host.steam_game_running() {
            self.inner.restart_pending.store(true, Ordering::Relaxed);
        } else {
            self.inner.restart_pending.store(false, Ordering::Relaxed);
            let fut = self.inner.host.restart_steam();
            tokio::spawn(async move {
                if let Err(e) = fut.await {
                    tracing::warn!("downloads: restarting Steam: {e}");
                }
            });
        }
    }

    /// The bridge's port: the one picked for it, else Steam's default (a Steam from before the port was picked).
    fn bridge_port(&self) -> u16 {
        match self.settings().steam_bridge_port {
            0 => self.inner.cfg.bridge_port,
            p => p,
        }
    }

    fn settings(&self) -> DlSettings {
        self.inner.settings.read().unwrap().clone()
    }

    fn changed(&self) {
        self.inner.dirty.notify_one();
    }

    fn bridge(&self) -> Option<SteamBridge> {
        let b = self.inner.bridge.lock().unwrap().clone()?;
        (b.status() == steam_bridge::Status::Connected).then_some(b)
    }

    fn bridge_state(&self) -> BridgeState {
        if self.inner.restart_pending.load(Ordering::Relaxed) {
            return BridgeState::RestartPending;
        }
        if !self.settings().steam_bridge {
            return BridgeState::Off;
        }
        if self.bridge().is_some() { BridgeState::On } else { BridgeState::Unavailable }
    }

    fn engine_cfg(&self) -> EngineConfig {
        let mut c = self.inner.cfg.engine.clone();
        c.connections = self.settings().connections.clamp(1, 32) as usize;
        c.limit = self.inner.limit.clone();
        c
    }

    fn epic_default_dir(&self) -> PathBuf {
        fsutil::expand_home(&self.settings().epic_default_dir, &self.inner.cfg.home)
    }

    // ------------------------------------------------------------------------------------------------ snapshot

    /// What `downloads_snapshot` and `downloads-changed` carry.
    pub fn snapshot(&self) -> DlSnapshot {
        let rows = self.inner.db.list().unwrap_or_default();
        let running = self.inner.running.lock().unwrap().clone();
        let bridge = self.bridge_state() == BridgeState::On;
        let speeds = self.inner.speeds.lock().unwrap();
        let steam = self.inner.steam.lock().unwrap();
        let t = now();

        let item = |r: &Row| -> Option<DlItem> {
            let info = self.inner.host.game(&r.id);
            if r.store == DlStore::Steam && info.is_none() {
                return None;
            }
            let info = info.unwrap_or_else(|| GameInfo { name: r.id.split_once(':').map_or(r.id.as_str(), |x| x.1).to_string(), ..GameInfo::default() });
            let live = running.as_ref().filter(|x| x.id == r.id);
            let mut state = match r.state.as_str() {
                "starting" => DlState::Starting,
                "downloading" => DlState::Downloading,
                "verifying" => DlState::Verifying,
                "installing" => DlState::Installing,
                "paused" => DlState::Paused,
                "done" => DlState::Done,
                "error" => DlState::Error,
                _ => DlState::Queued,
            };
            let (mut done, mut total) = (r.bytes_done, r.bytes_total);
            let sp = speeds.get(&r.id);
            let (mut bps, mut left) = (sp.and_then(|s| s.bps), None);
            if let Some(l) = live {
                state = match l.progress.phase() {
                    Phase::Starting => DlState::Starting,
                    Phase::Downloading => DlState::Downloading,
                    Phase::Verifying => DlState::Verifying,
                    Phase::Installing => DlState::Installing,
                };
                done = l.progress.done.load(Ordering::Relaxed);
                total = Some(l.progress.total.load(Ordering::Relaxed)).filter(|t| *t > 0).or(total);
            }
            if r.store == DlStore::Steam
                && let Some(app) = steam_app(&r.id).and_then(|a| steam.apps.get(&a))
            {
                done = app.done;
                total = app.total.or(total);
                bps = app.bytes_per_sec.or(bps);
                left = app.seconds_left;
            }
            if state != DlState::Downloading {
                bps = None;
            }
            if left.is_none()
                && let (Some(b), Some(t)) = (bps.filter(|b| *b > 0), total)
            {
                left = Some(t.saturating_sub(done) / b);
            }
            let epic = r.store == DlStore::Epic;
            let open = !matches!(state, DlState::Done);
            Some(DlItem {
                game: r.id.clone(),
                store: r.store,
                kind: r.kind,
                state,
                name: info.name,
                art: info.art,
                bytes_done: done,
                bytes_total: total,
                bytes_per_sec: bps,
                seconds_left: if state == DlState::Downloading { left } else { None },
                speed_history: sp.map(|s| s.history.iter().copied().collect()).unwrap_or_default(),
                target_dir: r.target_dir.clone(),
                position: r.position,
                can_pause: open && !matches!(state, DlState::Paused | DlState::Error) && (epic || bridge),
                can_reorder: open && (epic || bridge),
                can_cancel: open && (epic || bridge) && r.state != AVAILABLE,
                error: r.error.clone().filter(|_| state == DlState::Error),
                finished_at: r.finished_at.filter(|_| state == DlState::Done),
            })
        };

        let mut done: Vec<(i64, DlItem)> = rows
            .iter()
            .filter(|r| r.state == "done" && r.finished_at.is_some_and(|f| f > t - 86_400))
            .filter_map(|r| Some((r.finished_at.unwrap_or(0), item(r)?)))
            .collect();
        done.sort_by_key(|d| std::cmp::Reverse(d.0));
        let done: Vec<DlItem> = done.into_iter().take(10).map(|x| x.1).collect();

        let open: Vec<&Row> = rows.iter().filter(|r| r.state != "done").collect();
        // Active: the running Epic download, else the Steam download that is moving.
        let active_id = running
            .as_ref()
            .map(|r| r.id.clone())
            .or_else(|| open.iter().filter(|r| r.store == DlStore::Steam && r.state == "downloading").min_by_key(|r| r.position).map(|r| r.id.clone()));
        let active = active_id.as_ref().and_then(|id| open.iter().find(|r| &r.id == id)).and_then(|r| item(r));

        let rest = |kind: DlKind| -> Vec<DlItem> {
            let pick = |store: DlStore| -> Vec<&Row> {
                let mut v: Vec<&Row> = open.iter().copied().filter(|r| r.store == store && r.kind == kind && Some(&r.id) != active_id.as_ref()).collect();
                v.sort_by_key(|r| (r.position, r.added_at));
                v
            };
            // Epic and Steam each in their own order, interleaved by when they were added.
            let (e, s) = (pick(DlStore::Epic), pick(DlStore::Steam));
            let (mut i, mut j) = (0, 0);
            let mut out = Vec::new();
            while i < e.len() || j < s.len() {
                let take_e = j >= s.len() || (i < e.len() && e[i].added_at <= s[j].added_at);
                let r = if take_e {
                    i += 1;
                    e[i - 1]
                } else {
                    j += 1;
                    s[j - 1]
                };
                if let Some(it) = item(r) {
                    out.push(it);
                }
            }
            out
        };
        DlSnapshot {
            active,
            queued: rest(DlKind::Install),
            updates: rest(DlKind::Update),
            done,
            paused_all: self.inner.paused_all.load(Ordering::Relaxed),
            steam_bridge: self.bridge_state(),
        }
    }

    /// The snapshot for the Downloads page; also starts an Epic update check when the last one is over 10 minutes old.
    pub fn snapshot_on_demand(&self) -> DlSnapshot {
        // The first check is the scheduled one (shortly after start), so opening grapple costs no Epic call.
        let stale = self.inner.last_update_check.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(600));
        if stale {
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.check_updates().await {
                    tracing::info!("downloads: update check: {e}");
                }
            });
        }
        self.snapshot()
    }

    async fn emitter(&self) {
        loop {
            self.inner.dirty.notified().await;
            let snap = self.snapshot();
            self.inner.host.emit(&snap);
            tokio::time::sleep(self.inner.cfg.emit_every).await;
        }
    }

    async fn ticker(&self) {
        let mut last_sample = Instant::now();
        let mut last_save = Instant::now();
        let mut last_seen: Option<(u64, Phase)> = None;
        loop {
            tokio::time::sleep(self.inner.cfg.emit_every).await;
            let running = self.inner.running.lock().unwrap().clone();
            let steam_moving: Vec<(String, Option<u64>)> = self
                .inner
                .steam
                .lock()
                .unwrap()
                .apps
                .values()
                .filter(|a| a.phase == SteamPhase::Downloading)
                .map(|a| (format!("steam:{}", a.appid), a.bytes_per_sec))
                .collect();
            if running.is_none() && steam_moving.is_empty() {
                last_seen = None;
                continue;
            }
            if let Some(r) = &running {
                let now_seen = (r.progress.done.load(Ordering::Relaxed), r.progress.phase());
                if last_seen != Some(now_seen) {
                    self.changed();
                }
                last_seen = Some(now_seen);
            }
            if last_sample.elapsed() >= Duration::from_secs(1) {
                last_sample = Instant::now();
                let mut speeds = self.inner.speeds.lock().unwrap();
                if let Some(r) = &running {
                    let net = r.progress.network.load(Ordering::Relaxed);
                    let s = speeds.entry(r.id.clone()).or_default();
                    if let Some((b, t)) = s.last {
                        let bps = (net.saturating_sub(b) as f64 / t.elapsed().as_secs_f64().max(0.001)) as u64;
                        s.bps = Some(bps);
                        push(&mut s.history, bps);
                    }
                    s.last = Some((net, Instant::now()));
                }
                for (id, bps) in &steam_moving {
                    let s = speeds.entry(id.clone()).or_default();
                    if let Some(b) = bps {
                        s.bps = Some(*b);
                        push(&mut s.history, *b);
                    }
                }
                drop(speeds);
                self.changed();
            }
            if let Some(r) = &running
                && last_save.elapsed() >= Duration::from_secs(5)
            {
                last_save = Instant::now();
                let total = Some(r.progress.total.load(Ordering::Relaxed)).filter(|t| *t > 0);
                let _ = self.inner.db.set_progress(&r.id, r.progress.done.load(Ordering::Relaxed), total);
            }
        }
    }

    // ------------------------------------------------------------------------------------------------ Epic runner

    fn playing_blocks(&self, kind: DlKind) -> bool {
        let s = self.settings();
        let playing = self.inner.playing.load(Ordering::Relaxed);
        playing && (s.pause_while_playing || (kind == DlKind::Update && s.update_only_when_idle))
    }

    fn next_epic(&self) -> Option<Row> {
        if self.inner.paused_all.load(Ordering::Relaxed) {
            return None;
        }
        let rows = self.inner.db.list().ok()?;
        rows.into_iter().filter(|r| r.store == DlStore::Epic && r.state == "queued").find(|r| !self.playing_blocks(r.kind))
    }

    async fn runner(&self) {
        loop {
            match self.next_epic() {
                Some(row) => self.run_one(row).await,
                None => {
                    tokio::select! {
                        _ = self.inner.wake.notified() => {}
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    }
                }
            }
        }
    }

    fn fail(&self, id: &str, e: &Error) {
        tracing::info!("downloads: {id}: {e}");
        let _ = self.inner.db.set_state(id, "error", Some(&e.short()));
        self.changed();
    }

    async fn fetch_build(&self, meta: &legendary::EpicMeta) -> Result<Build> {
        if let Some((at, b)) = self.inner.plans.lock().unwrap().remove(&meta.app_name)
            && at.elapsed() < Duration::from_secs(900)
        {
            return Ok(b);
        }
        let token = self.inner.cfg.tokens.token().await?;
        let loc = self.inner.epic.manifest_location(&token, &meta.namespace, &meta.catalog_item_id, &meta.app_name).await?;
        let bytes = self.inner.epic.manifest(&loc).await?;
        Build::new(bytes, loc.base_urls)
    }

    /// The game's own folder for a row.
    fn install_path(&self, row: &Row, meta: &legendary::EpicMeta) -> Result<PathBuf> {
        match row.kind {
            DlKind::Install => Ok(fsutil::expand_home(&row.target_dir, &self.inner.cfg.home).join(legendary::folder_name(meta))),
            DlKind::Update => legendary::installed_entry(&self.inner.cfg.legendary_dir, &meta.app_name)?
                .and_then(|e| e.get("install_path").and_then(serde_json::Value::as_str).map(PathBuf::from))
                .ok_or_else(|| Error::Invalid("not installed".into())),
        }
    }

    async fn run_one(&self, row: Row) {
        let id = row.id.clone();
        let Some(app) = epic_app(&id).map(str::to_string) else {
            let _ = self.inner.db.remove(&id);
            return;
        };
        let _ = self.inner.db.set_state(&id, "starting", None);
        let running = Arc::new(Running { id: id.clone(), control: Control::new(), progress: Progress::new(), requeue: AtomicBool::new(false) });
        *self.inner.running.lock().unwrap() = Some(running.clone());
        self.changed();

        let result: Result<Outcome> = async {
            let meta = legendary::read_metadata(&self.inner.cfg.legendary_dir, &app)?;
            let install_path = self.install_path(&row, &meta)?;
            let build = if engine::saved(&install_path).is_some() { None } else { Some(self.fetch_build(&meta).await?) };
            let _ = self.inner.db.set_state(&id, "downloading", None);
            let kind = if row.kind == DlKind::Update { Kind::Update } else { Kind::Install };
            let job = Job { kind, meta, install_path, legendary_dir: self.inner.cfg.legendary_dir.clone(), register: true };
            engine::run(&self.engine_cfg(), &job, build, running.control.clone(), running.progress.clone()).await
        }
        .await;

        *self.inner.running.lock().unwrap() = None;
        let done = running.progress.done.load(Ordering::Relaxed);
        let total = Some(running.progress.total.load(Ordering::Relaxed)).filter(|t| *t > 0);
        match result {
            Ok(Outcome::Done { .. }) => {
                let _ = self.inner.db.set_progress(&id, total.unwrap_or(done), total);
                let _ = self.inner.db.finish(&id, now());
                self.inner.speeds.lock().unwrap().remove(&id);
                if row.kind == DlKind::Install && self.settings().notify_ready {
                    let name = self.inner.host.game(&id).map(|g| g.name).unwrap_or(app.clone());
                    self.inner.host.notify_ready(&name);
                }
                self.inner.host.games_changed();
            }
            Ok(Outcome::Paused) => {
                let _ = self.inner.db.set_progress(&id, done, total);
                let to = if running.requeue.load(Ordering::Relaxed) { "queued" } else { "paused" };
                let _ = self.inner.db.set_state(&id, to, None);
            }
            Ok(Outcome::Cancelled) => {
                let _ = self.inner.db.remove(&id);
                self.inner.speeds.lock().unwrap().remove(&id);
            }
            Err(e) => {
                let _ = self.inner.db.set_progress(&id, done, total);
                self.fail(&id, &e);
            }
        }
        let _ = self.inner.db.prune_done(now());
        self.changed();
    }

    fn running_is(&self, id: &str) -> Option<Arc<Running>> {
        self.inner.running.lock().unwrap().clone().filter(|r| r.id == id)
    }

    // ------------------------------------------------------------------------------------------------ Epic updates

    // ------------------------------------------------------------------------------------------------ Epic overlay

    /// Where grapple keeps Epic's in-game overlay (next to its downloads database).
    pub fn eos_overlay_dir(&self) -> PathBuf {
        self.inner.cfg.db_path.parent().map(Path::to_path_buf).unwrap_or_default().join("eos-overlay")
    }

    /// The overlay folder, when a complete overlay is installed there.
    pub fn eos_overlay_installed(&self) -> Option<PathBuf> {
        let dir = self.eos_overlay_dir();
        crate::eos_overlay::installed_version(&dir).map(|_| dir)
    }

    /// Install Epic's overlay, or update it to Epic's current build. Returns the build installed.
    pub async fn eos_overlay_install(&self) -> Result<String> {
        let _one = self.inner.overlay.lock().await;
        let token = self.inner.cfg.tokens.token().await?;
        let dir = self.eos_overlay_dir();
        crate::eos_overlay::install(&self.engine_cfg(), &self.inner.epic, &token, &dir, &self.inner.cfg.legendary_dir).await
    }

    /// Compare every installed Epic game's version with Epic's current build (Epic's list, or legendary's
    /// `assets.json` when offline) and queue (auto update) or list the updates.
    pub async fn check_updates(&self) -> Result<()> {
        *self.inner.last_update_check.lock().unwrap() = Some(Instant::now());
        let ld = self.inner.cfg.legendary_dir.clone();
        let installed = blocking(move || legendary::read_installed(&ld)).await?;
        if installed.is_empty() {
            return Ok(());
        }
        let assets = match self.inner.cfg.tokens.token().await {
            Ok(t) => match self.inner.epic.assets(&t).await {
                Ok(a) => a,
                Err(Error::SignedOut) => return Err(Error::SignedOut),
                Err(_) => legendary::read_assets(&self.inner.cfg.legendary_dir),
            },
            Err(Error::SignedOut) => return Err(Error::SignedOut),
            Err(_) => legendary::read_assets(&self.inner.cfg.legendary_dir),
        };
        let auto = self.settings().auto_update;
        let mut pos = self.inner.db.max_position()?;
        for (app, entry) in &installed {
            let id = format!("epic:{app}");
            let version = entry.get("version").and_then(serde_json::Value::as_str).unwrap_or_default();
            let platform = entry.get("platform").and_then(serde_json::Value::as_str).unwrap_or("Windows");
            if !platform.starts_with("Win") || entry.get("is_dlc").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                continue;
            }
            let Some(asset) = assets.iter().find(|a| &a.app_name == app) else { continue };
            let existing = self.inner.db.get(&id)?;
            if asset.build_version == version {
                // Up to date (Heroic may have updated it): drop a stale update row.
                if let Some(r) = existing.filter(|r| r.kind == DlKind::Update && matches!(r.state.as_str(), "queued" | AVAILABLE)) {
                    self.inner.db.remove(&r.id)?;
                }
                continue;
            }
            if existing.as_ref().is_some_and(|r| r.state != "done") {
                continue;
            }
            let parent = entry.get("install_path").and_then(serde_json::Value::as_str).map(PathBuf::from).and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
            pos += 1;
            self.inner.db.put(&Row {
                id,
                store: DlStore::Epic,
                kind: DlKind::Update,
                target_dir: tilde(&parent, &self.inner.cfg.home),
                position: pos,
                state: if auto { "queued".into() } else { AVAILABLE.into() },
                bytes_done: 0,
                bytes_total: None,
                added_at: now(),
                finished_at: None,
                error: None,
            })?;
        }
        self.inner.wake.notify_one();
        self.changed();
        Ok(())
    }

    /// "Check for update" on one installed game. Epic: asks Epic now (an update found is queued when updating
    /// automatically). Steam: Steam's own answer, which it keeps current while it runs; its files are reread first.
    pub async fn check_update(&self, id: &str) -> Result<UpdateCheck> {
        if steam_app(id).is_some() {
            self.inner.steam_wake.notify_one();
            tokio::time::sleep(Duration::from_millis(1500)).await;
        } else if epic_app(id).is_some() {
            self.check_updates().await?;
        } else {
            return Err(Error::Invalid("not a Steam or Epic game".into()));
        }
        Ok(match self.inner.db.get(id)?.filter(|r| r.kind == DlKind::Update).map(|r| r.state) {
            None => UpdateCheck::UpToDate,
            Some(s) => match s.as_str() {
                "done" => UpdateCheck::UpToDate,
                AVAILABLE => UpdateCheck::Available,
                "queued" => UpdateCheck::Queued,
                _ => UpdateCheck::Downloading,
            },
        })
    }

    // ------------------------------------------------------------------------------------------------ Steam

    async fn steam_loop(&self) {
        let Some(root) = self.inner.cfg.steam_root.clone() else { return };
        let watch = Arc::new(Mutex::new(SteamWatch::new(root, self.inner.cfg.steam_log_from_start)));
        loop {
            let w = watch.clone();
            let res = tokio::task::spawn_blocking(move || {
                let mut w = w.lock().unwrap();
                let r = w.poll();
                (r, w.busy())
            })
            .await;
            let Ok((res, busy)) = res else { break };
            self.apply_steam(res.apps, &res.finished);
            let wait = if busy || self.bridge().is_some_and(|b| b.items().iter().any(|i| !i.completed)) {
                self.inner.cfg.steam_poll_busy
            } else {
                self.inner.cfg.steam_poll_idle
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.inner.steam_wake.notified() => {}
            }
        }
    }

    /// Merge the watcher's view with the bridge's live numbers, then mirror it into the queue.
    fn apply_steam(&self, mut apps: Vec<SteamApp>, finished: &[u32]) {
        let bridge = self.bridge();
        let overview = self.inner.steam.lock().unwrap().overview.clone();
        if let Some(b) = &bridge {
            for it in b.items().into_iter().filter(|i| !i.completed) {
                let phase = if it.paused {
                    SteamPhase::Paused
                } else if it.active {
                    SteamPhase::Downloading
                } else {
                    SteamPhase::Queued
                };
                match apps.iter_mut().find(|a| a.appid == it.appid) {
                    Some(a) => {
                        a.phase = phase;
                        a.update_available = false;
                        a.queue_index = u32::try_from(it.queue_index).ok().or(a.queue_index);
                        if it.total > 0 {
                            a.total = Some(it.total);
                            a.done = it.downloaded;
                        }
                    }
                    None => apps.push(SteamApp {
                        appid: it.appid,
                        name: String::new(),
                        kind: if it.buildid != 0 { SteamKind::Update } else { SteamKind::Install },
                        phase,
                        done: it.downloaded,
                        total: (it.total > 0).then_some(it.total),
                        bytes_per_sec: None,
                        seconds_left: None,
                        queue_index: u32::try_from(it.queue_index).ok(),
                        library: PathBuf::new(),
                        update_available: false,
                    }),
                }
            }
            if let Some(o) = overview.filter(|o| o.appid != 0 && o.state != "None")
                && let Some(a) = apps.iter_mut().find(|a| a.appid == o.appid)
            {
                a.bytes_per_sec = Some(o.bytes_per_sec);
                a.seconds_left = (o.seconds_left > 0).then_some(o.seconds_left);
                if o.to_download > 0 {
                    a.done = o.downloaded;
                    a.total = Some(o.to_download);
                }
                if !o.paused {
                    a.phase = SteamPhase::Downloading;
                }
            }
        }
        apps.retain(|a| self.inner.host.game(&format!("steam:{}", a.appid)).is_some());
        let t = now();
        let db = &self.inner.db;
        let rows: Vec<Row> = db.list().unwrap_or_default().into_iter().filter(|r| r.store == DlStore::Steam).collect();
        let mut games_changed = false;
        for a in &apps {
            let id = format!("steam:{}", a.appid);
            let old = rows.iter().find(|r| r.id == id);
            let state = if a.update_available {
                AVAILABLE
            } else {
                match a.phase {
                    SteamPhase::Downloading => "downloading",
                    SteamPhase::Paused => "paused",
                    SteamPhase::Queued => "queued",
                }
            };
            let target = if a.library.as_os_str().is_empty() { old.map(|r| r.target_dir.clone()).unwrap_or_default() } else { tilde(&a.library, &self.inner.cfg.home) };
            let row = Row {
                id: id.clone(),
                store: DlStore::Steam,
                kind: if a.kind == SteamKind::Update { DlKind::Update } else { DlKind::Install },
                target_dir: target,
                position: a.queue_index.map(i64::from).or(old.map(|r| r.position)).unwrap_or(1000),
                state: state.into(),
                bytes_done: a.done,
                bytes_total: a.total,
                added_at: old.filter(|r| r.state != "done").map(|r| r.added_at).unwrap_or(t),
                finished_at: None,
                error: None,
            };
            if old != Some(&row) {
                let _ = db.put(&row);
            }
        }
        for r in rows.iter().filter(|r| r.state != "done") {
            let Some(appid) = steam_app(&r.id) else { continue };
            if apps.iter().any(|a| a.appid == appid) {
                continue;
            }
            if finished.contains(&appid) || r.state == "downloading" {
                let _ = db.finish(&r.id, t);
                games_changed = true;
                if r.kind == DlKind::Install && self.settings().notify_ready
                    && let Some(g) = self.inner.host.game(&r.id) {
                        self.inner.host.notify_ready(&g.name);
                    }
            } else if !(r.state == "queued" && r.added_at > t - 600) {
                // Gone from Steam's list (cancelled there, or a request Steam never took).
                let _ = db.remove(&r.id);
            }
        }
        self.inner.steam.lock().unwrap().apps = apps.into_iter().map(|a| (a.appid, a)).collect();
        if games_changed {
            let _ = db.prune_done(t);
            self.inner.host.games_changed();
        }
        self.changed();
    }

    fn spawn_bridge(&self) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let b = SteamBridge::spawn(self.bridge_port(), tx);
        if let Some(old) = self.inner.bridge.lock().unwrap().replace(b) {
            old.stop();
        }
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    BridgeEvent::Overview(o) => me.inner.steam.lock().unwrap().overview = Some(o),
                    BridgeEvent::Connected
                        // Steam was paused for a game while the bridge was away: keep that promise.
                        if me.inner.playing.load(Ordering::Relaxed) && me.settings().pause_while_playing => {
                            me.pause_steam_for_play().await;
                        }
                    _ => {}
                }
                me.inner.steam_wake.notify_one();
                me.changed();
            }
        });
    }

    async fn steam_call(&self, c: Call) -> Result<()> {
        let b = self.bridge().ok_or_else(|| Error::Steam("bridge off".into()))?;
        b.call(c).await.map(|_| ())
    }

    async fn pause_steam_for_play(&self) {
        if self.steam_call(Call::EnableAllDownloads(false)).await.is_ok() {
            self.inner.paused_steam_for_play.store(true, Ordering::Relaxed);
        }
    }

    // ------------------------------------------------------------------------------------------------ commands

    fn row(&self, id: &str) -> Result<Row> {
        self.inner.db.get(id)?.ok_or_else(|| Error::Invalid("not in the queue".into()))
    }

    pub async fn pause(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            return self.steam_call(Call::PauseAppUpdate(appid)).await;
        }
        match self.running_is(id) {
            Some(r) => r.control.pause(),
            None => {
                let r = self.row(id)?;
                if r.state == "queued" {
                    self.inner.db.set_state(id, "paused", None)?;
                }
            }
        }
        self.changed();
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            return self.steam_call(Call::ResumeAppUpdate(appid)).await;
        }
        let r = self.row(id)?;
        if matches!(r.state.as_str(), "paused" | "error" | AVAILABLE) {
            self.inner.db.set_state(id, "queued", None)?;
        }
        self.inner.wake.notify_one();
        self.changed();
        Ok(())
    }

    pub async fn cancel(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            // Off Steam's list; a new install also goes away with its partly downloaded files (an update only
            // stops, the game stays playable).
            self.steam_call(Call::RemoveFromDownloadList(appid)).await?;
            if self.row(id).is_ok_and(|r| r.kind == DlKind::Install) {
                self.steam_call(Call::OpenUninstallWizard(appid)).await?;
            }
            let _ = self.inner.db.remove(id);
            self.changed();
            return Ok(());
        }
        if let Some(r) = self.running_is(id) {
            r.control.cancel();
            return Ok(());
        }
        let r = self.row(id)?;
        if let Some(app) = epic_app(id)
            && let Ok(meta) = legendary::read_metadata(&self.inner.cfg.legendary_dir, app)
            && let Ok(path) = self.install_path(&r, &meta)
        {
            let kind = if r.kind == DlKind::Update { Kind::Update } else { Kind::Install };
            blocking(move || {
                engine::discard_blocking(&path, kind);
                Ok(())
            })
            .await?;
        }
        self.inner.db.remove(id)?;
        self.changed();
        Ok(())
    }

    /// Move to the top and start it (Epic: the running Epic download makes room and stays queued).
    pub async fn start_now(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            let r = self.row(id)?;
            if r.state == AVAILABLE {
                self.steam_call(Call::QueueAppUpdate(appid)).await?;
            }
            return self.steam_call(Call::SetQueueIndex(appid, 0)).await;
        }
        let r = self.row(id)?;
        self.inner.db.set_position(id, self.inner.db.min_position()? - 1)?;
        if matches!(r.state.as_str(), "paused" | "error" | AVAILABLE | "queued") {
            self.inner.db.set_state(id, "queued", None)?;
        }
        if self.inner.paused_all.swap(false, Ordering::Relaxed) {
            self.inner.db.set_setting(PAUSED_ALL_KEY, &false)?;
        }
        let other = self.inner.running.lock().unwrap().clone().filter(|x| x.id != id);
        if let Some(o) = other {
            o.requeue.store(true, Ordering::Relaxed);
            o.control.pause();
        }
        self.inner.wake.notify_one();
        self.changed();
        Ok(())
    }

    /// New order for some items (drag and drop). Epic among Epic; Steam among Steam with the bridge.
    pub async fn reorder(&self, ids: &[String]) -> Result<()> {
        let base = self.inner.db.min_position()?;
        let mut steam_i = 0u32;
        for (i, id) in ids.iter().enumerate() {
            if let Some(appid) = steam_app(id) {
                if self.bridge().is_some() {
                    self.steam_call(Call::SetQueueIndex(appid, steam_i)).await?;
                    steam_i += 1;
                }
            } else if epic_app(id).is_some() && self.inner.db.get(id)?.is_some() {
                self.inner.db.set_position(id, base + i as i64)?;
            }
        }
        self.changed();
        Ok(())
    }

    pub async fn pause_all(&self, paused: bool) -> Result<()> {
        self.inner.paused_all.store(paused, Ordering::Relaxed);
        self.inner.db.set_setting(PAUSED_ALL_KEY, &paused)?;
        if paused {
            // Shown as paused; "Resume all" queues every paused download again.
            let r = self.inner.running.lock().unwrap().clone();
            if let Some(r) = r {
                r.control.pause();
            }
        } else {
            for r in self.inner.db.list()? {
                if r.store == DlStore::Epic && r.state == "paused" {
                    self.inner.db.set_state(&r.id, "queued", None)?;
                }
            }
        }
        if self.bridge().is_some() {
            let _ = self.steam_call(Call::EnableAllDownloads(!paused)).await;
        }
        self.inner.wake.notify_one();
        self.changed();
        Ok(())
    }

    pub async fn update_now(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            self.steam_call(Call::QueueAppUpdate(appid)).await?;
            return self.steam_call(Call::SetQueueIndex(appid, 0)).await;
        }
        if self.inner.db.get(id)?.is_none() {
            let _ = self.check_updates().await;
        }
        self.start_now(id).await
    }

    /// Games started or stopped (the app calls this on every launch event).
    pub async fn set_playing(&self, playing: bool, steam_game: bool) {
        let was = self.inner.playing.swap(playing, Ordering::Relaxed);
        let s = self.settings();
        if playing && !was && s.pause_while_playing {
            let r = self.inner.running.lock().unwrap().clone();
            if let Some(r) = r {
                r.requeue.store(true, Ordering::Relaxed);
                r.control.pause();
            }
            self.pause_steam_for_play().await;
        } else if playing && !was && s.update_only_when_idle {
            let r = self.inner.running.lock().unwrap().clone();
            if let Some(r) = r.filter(|r| self.inner.db.get(&r.id).ok().flatten().is_some_and(|x| x.kind == DlKind::Update)) {
                r.requeue.store(true, Ordering::Relaxed);
                r.control.pause();
            }
        }
        if !playing && was && self.inner.paused_steam_for_play.swap(false, Ordering::Relaxed) && !self.inner.paused_all.load(Ordering::Relaxed) {
            let _ = self.steam_call(Call::EnableAllDownloads(true)).await;
        }
        if !steam_game && self.inner.restart_pending.swap(false, Ordering::Relaxed) {
            let fut = self.inner.host.restart_steam();
            tokio::spawn(async move {
                if let Err(e) = fut.await {
                    tracing::warn!("downloads: restarting Steam: {e}");
                }
            });
        }
        self.inner.wake.notify_one();
        self.changed();
    }

    // ------------------------------------------------------------------------------------------------ install

    fn steam_libraries(&self) -> Vec<PathBuf> {
        self.inner.cfg.steam_root.as_deref().map(grapple_scan::steam_library_dirs).unwrap_or_default()
    }

    /// libraryfolders.vdf: (key, path).
    fn steam_folder_keys(&self) -> Vec<(u32, PathBuf)> {
        let Some(root) = self.inner.cfg.steam_root.as_deref() else { return Vec::new() };
        let text = ["steamapps", "config"].iter().find_map(|d| std::fs::read_to_string(root.join(d).join("libraryfolders.vdf")).ok()).unwrap_or_default();
        let v = grapple_scan::vdf::Vdf::parse(&text);
        v.get("libraryfolders")
            .map(|lf| lf.entries().iter().filter_map(|(k, val)| Some((k.parse().ok()?, PathBuf::from(val.str_at(&["path"]).or(val.as_str())?)))).collect())
            .unwrap_or_default()
    }

    /// Folders an Epic game can go to: the default, the parents of installed Epic games, and a `Games` folder on
    /// each other drive with a Steam library. One per drive.
    fn epic_dirs(&self) -> Vec<PathBuf> {
        let home = &self.inner.cfg.home;
        let mut cands = vec![self.epic_default_dir()];
        if let Ok(all) = legendary::read_installed(&self.inner.cfg.legendary_dir) {
            for e in all.values() {
                if let Some(p) = e.get("install_path").and_then(serde_json::Value::as_str).map(PathBuf::from).and_then(|p| p.parent().map(Path::to_path_buf)) {
                    cands.push(p);
                }
            }
        }
        for lib in self.steam_libraries() {
            if let Some(mp) = fsutil::mount_point(&lib)
                && fsutil::device(&mp) != fsutil::device(home)
            {
                cands.push(mp.join("Games"));
            }
        }
        let mut out: Vec<PathBuf> = Vec::new();
        let mut devs = Vec::new();
        for c in cands {
            if !c.is_absolute() || out.contains(&c) {
                continue;
            }
            let d = fsutil::device(&c);
            if d.is_some() && devs.contains(&d) {
                continue;
            }
            devs.push(d);
            out.push(c);
        }
        out
    }

    fn target(&self, dir: &Path, need: Option<u64>, is_default: bool) -> InstallTarget {
        let space = (self.inner.cfg.engine.free_space)(dir).unwrap_or_default();
        let home = &self.inner.cfg.home;
        InstallTarget {
            path: tilde(dir, home),
            label: fsutil::drive_label(dir, home),
            free_bytes: space.free,
            total_bytes: space.total,
            enough: need.is_none_or(|n| space.free >= n + engine::GB),
            is_default,
        }
    }

    /// What the install sheet shows: size, drives, and (Epic on Linux) the Proton choices.
    pub async fn install_plan(&self, id: &str) -> Result<InstallPlan> {
        if let Some(appid) = steam_app(id) {
            let size = match self.bridge() {
                Some(b) => b.install_size(appid).await.unwrap_or(None),
                None => None,
            };
            let libs = self.steam_libraries();
            let default = self.settings().steam_default_dir.map(|d| fsutil::expand_home(&d, &self.inner.cfg.home)).filter(|d| libs.contains(d)).or_else(|| libs.first().cloned());
            let targets = libs.iter().map(|l| self.target(l, size, Some(l) == default.as_ref())).collect();
            return Ok(InstallPlan { game: id.into(), store: DlStore::Steam, size_bytes: size, targets, protons: None });
        }
        let app = epic_app(id).ok_or_else(|| Error::Invalid("not a game grapple can install".into()))?;
        let meta = legendary::read_metadata(&self.inner.cfg.legendary_dir, app)?;
        let b = self.fetch_build(&meta).await?;
        let size: u64 = b.manifest.chunks.iter().map(|c| c.download_size()).sum();
        let need = b.manifest.install_size();
        self.inner.plans.lock().unwrap().insert(app.to_string(), (Instant::now(), b));
        let default = self.epic_default_dir();
        let targets = self.epic_dirs().iter().map(|d| self.target(d, Some(need), *d == default)).collect();
        let protons = if cfg!(target_os = "linux") { Some(self.inner.host.protons()) } else { None };
        Ok(InstallPlan { game: id.into(), store: DlStore::Epic, size_bytes: Some(size), targets, protons })
    }

    /// Queue an install into `target_dir` (a path from the plan; `~/…` allowed).
    pub async fn install_start(&self, id: &str, target_dir: &str) -> Result<()> {
        let dir = fsutil::expand_home(target_dir.trim(), &self.inner.cfg.home);
        if !dir.is_absolute() {
            return Err(Error::Invalid("choose a drive".into()));
        }
        if let Some(r) = self.inner.db.get(id)?
            && r.state != "done"
        {
            return Err(Error::Invalid("already in the queue".into()));
        }
        let t = now();
        let mut row = Row {
            id: id.into(),
            store: DlStore::Epic,
            kind: DlKind::Install,
            target_dir: tilde(&dir, &self.inner.cfg.home),
            position: self.inner.db.max_position()? + 1,
            state: "queued".into(),
            bytes_done: 0,
            bytes_total: None,
            added_at: t,
            finished_at: None,
            error: None,
        };
        if let Some(appid) = steam_app(id) {
            row.store = DlStore::Steam;
            match self.bridge() {
                Some(b) => {
                    let index = b.folder_index(&dir, &self.steam_folder_keys()).await.unwrap_or(0);
                    b.install(appid, index, self.inner.cfg.bridge_install_wait).await?;
                }
                None => self.inner.host.steam_open_url(&format!("steam://install/{appid}"))?,
            }
            self.inner.db.put(&row)?;
            self.inner.steam_wake.notify_one();
            self.changed();
            return Ok(());
        }
        let app = epic_app(id).ok_or_else(|| Error::Invalid("not a game grapple can install".into()))?;
        let meta = legendary::read_metadata(&self.inner.cfg.legendary_dir, app)?;
        if legendary::installed_entry(&self.inner.cfg.legendary_dir, app)?.is_some() {
            return Err(Error::Invalid("already installed".into()));
        }
        if let Some(b) = self.inner.plans.lock().unwrap().get(app) {
            row.bytes_total = Some(b.1.manifest.chunks.iter().map(|c| c.download_size()).sum());
        }
        let _ = meta;
        self.inner.db.put(&row)?;
        self.inner.wake.notify_one();
        self.changed();
        Ok(())
    }

    /// Epic: delete the game's folder and its `installed.json` entry (the UI asks first). Steam: Steam's own
    /// uninstall (no prompt with the bridge).
    pub async fn uninstall(&self, id: &str) -> Result<()> {
        if let Some(appid) = steam_app(id) {
            if self.bridge().is_some() {
                return self.steam_call(Call::OpenUninstallWizard(appid)).await;
            }
            return self.inner.host.steam_open_url(&format!("steam://uninstall/{appid}"));
        }
        let app = epic_app(id).ok_or_else(|| Error::Invalid("not a game grapple can uninstall".into()))?.to_string();
        if self.running_is(id).is_some() {
            return Err(Error::Invalid("it is downloading".into()));
        }
        let ld = self.inner.cfg.legendary_dir.clone();
        let entry = legendary::installed_entry(&ld, &app)?.ok_or_else(|| Error::Invalid("not installed".into()))?;
        let path = entry.get("install_path").and_then(serde_json::Value::as_str).map(PathBuf::from).ok_or_else(|| Error::Invalid("no install folder".into()))?;
        let home = self.inner.cfg.home.clone();
        let protected: Vec<PathBuf> = self.epic_dirs().into_iter().chain(self.steam_libraries()).chain([home.clone()]).collect();
        blocking(move || {
            if path.exists() {
                if !fsutil::deletable_game_dir(&path, &home) || protected.iter().any(|p| p == &path) {
                    return Err(Error::Invalid(format!("will not delete {}", path.display())));
                }
                std::fs::remove_dir_all(&path).map_err(|e| Error::file(&path, e))?;
            }
            legendary::remove_installed(&ld, &app)?;
            Ok(())
        })
        .await?;
        let _ = self.inner.db.remove(id);
        self.inner.host.games_changed();
        self.changed();
        Ok(())
    }

    // ------------------------------------------------------------------------------------------------ settings

    pub fn settings_get(&self) -> DlSettings {
        let mut s = self.settings();
        let home = &self.inner.cfg.home;
        s.choices = Some(DirChoices {
            steam: self.steam_libraries().iter().map(|p| tilde(p, home)).collect(),
            epic: self.epic_dirs().iter().map(|p| tilde(p, home)).collect(),
        });
        s
    }

    /// Save the settings. The bridge switch is not changed here (`steam_bridge_set` does that, with its restart).
    pub fn settings_set(&self, mut s: DlSettings) -> Result<DlSettings> {
        let cur = self.settings();
        s.choices = None;
        s.steam_bridge = cur.steam_bridge;
        s.steam_bridge_port = cur.steam_bridge_port;
        s.connections = s.connections.clamp(1, 32);
        if s.epic_default_dir.trim().is_empty() {
            s.epic_default_dir = cur.epic_default_dir.clone();
        }
        s.steam_default_dir = s.steam_default_dir.filter(|d| !d.trim().is_empty());
        self.inner.db.set_setting(SETTINGS_KEY, &s)?;
        self.inner.limit.set_rate(s.speed_limit_bytes);
        *self.inner.settings.write().unwrap() = s;
        self.inner.wake.notify_one();
        self.changed();
        Ok(self.settings_get())
    }

    /// "Steam download control": create or delete Steam's debugging file, restart Steam (now, or after the Steam
    /// game that runs), and start or stop the bridge.
    pub async fn steam_bridge_set(&self, on: bool) -> Result<DlSettings> {
        let root = self.inner.cfg.steam_root.clone().ok_or_else(|| Error::Invalid("Steam is not installed".into()))?;
        let file = root.join(steam_bridge::ENABLE_FILE);
        let had = file.exists();
        blocking(move || {
            if on {
                std::fs::write(&file, b"").map_err(|e| Error::file(&file, e))?;
            } else if file.exists() {
                std::fs::remove_file(&file).map_err(|e| Error::file(&file, e))?;
            }
            Ok(())
        })
        .await?;
        let mut s = self.settings();
        // A port of our own, so the bridge never meets another program on Steam's default 8080.
        let new_port = on && s.steam_bridge_port == 0;
        if new_port {
            s.steam_bridge_port = steam_bridge::free_loopback_port()?;
        }
        s.steam_bridge = on;
        s.choices = None;
        self.inner.host.set_steam_devtools_port(on.then_some(s.steam_bridge_port));
        self.inner.db.set_setting(SETTINGS_KEY, &s)?;
        *self.inner.settings.write().unwrap() = s;
        if on {
            self.spawn_bridge();
        } else if let Some(b) = self.inner.bridge.lock().unwrap().take() {
            b.stop();
        }
        if had != on || new_port {
            self.restart_steam_when_free();
        }
        self.changed();
        Ok(self.settings_get())
    }
}

fn push(h: &mut VecDeque<u64>, v: u64) {
    h.push_back(v);
    while h.len() > HISTORY {
        h.pop_front();
    }
}
