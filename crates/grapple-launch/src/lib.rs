//! Starting games, finding their processes, recording play sessions. See PLAN.md.
//!
//! * [`Launcher`] is the entry point the app uses (`launch`, `stop`, `running`, `spawn_watcher`).
//! * [`LaunchStrategy`] is how one store starts a game: [`steam::SteamStrategy`], [`epic::EpicStrategy`],
//!   [`manual::ManualStrategy`]. They only start things; tracking is shared (`proc`, this file).
//! * Games are recognised by markers in their processes' environments (`SteamAppId`, `GRAPPLE_GAME`,
//!   `HEROIC_APP_NAME`) or, for games Grapple spawned, by position in the process tree. `/proc` is the only source.
//!   On Windows the markers come from where a process's executable is instead ([`winproc`]).
//! * While nothing runs, umu's Steam Linux Runtime is kept current in the background ([`umu`]), so a launch
//!   never waits for it; a launch stops that update if it is still downloading.

pub mod env;
pub mod eos;
pub mod epic;
pub mod epic_auth;
pub mod error;
pub mod gamemode;
pub mod heroic;
pub mod legendary;
pub mod manual;
pub mod pads;
pub mod paths;
pub mod plan;
pub mod proc;
pub mod proton;
pub mod settings;
pub mod sink;
pub mod spawn;
pub mod steam;
pub mod strategy;
pub mod umu;
pub mod winproc;

pub use error::{LaunchError, Result};
pub use settings::{CloseSteam, LaunchSettings, Wrappers};
pub use sink::{MemorySink, SessionSink};
pub use strategy::{LaunchStrategy, StartCtx, Started};

use grapple_core::{Db, Game, GameId, LaunchEvent, SessionSource, Store};
use proc::{Matcher, Scanner, significant, steam_pid, store_of, terminate};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};
use strategy::STEP_RUNTIME;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;

/// A tracked game that shows no live process for this many polls in a row is over. (Launchers hand over to the
/// game executable within a second, wine helpers linger a few seconds: two polls ride over both.)
const GRACE_POLLS: u32 = 2;
/// Poll interval while at least one game is tracked.
const TRACK_POLL: Duration = Duration::from_secs(2);
/// While a launch waits for the game's process to appear.
const LAUNCH_POLL: Duration = Duration::from_millis(400);
/// How often the "still running" time is saved (for crash recovery).
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// A launch through umu that has said nothing about its runtime for this long is past its setup (an umu
/// that prints differently than 1.4.0 still starts like before).
const UMU_QUIET: Duration = Duration::from_secs(10);
/// A runtime download that takes longer than this fails the launch.
const RUNTIME_DOWNLOAD_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Background runtime updates: the first look this long after Grapple starts, then every `RUNTIME_UPDATE_POLL`
/// (a check is due at most daily, see [`umu::UPDATE_EVERY`]).
const RUNTIME_UPDATE_START: Duration = Duration::from_secs(120);
const RUNTIME_UPDATE_POLL: Duration = Duration::from_secs(15 * 60);
/// A launch waits at most this long for a background update that is already unpacking.
const RUNTIME_INSTALL_WAIT: Duration = Duration::from_secs(10 * 60);

struct Tracked {
    session_id: i64,
    started_at: i64,
    matcher: Matcher,
    members: HashSet<u32>,
    missing: u32,
    last_alive: i64,
    /// Channel of the `launch` call (None for detected games).
    events: Option<Sender<LaunchEvent>>,
    /// GameMode is asked for the game's processes as they appear (games Grapple started).
    gamemode: Option<GameModeTrack>,
}

struct GameModeTrack {
    req: gamemode::Request,
    /// Pids already registered.
    done: HashSet<u32>,
}

/// The background runtime update that is running now.
#[derive(Clone)]
struct UpdaterRun {
    pid: u32,
    log: PathBuf,
    exit: watch::Receiver<Option<i32>>,
}

struct State {
    scanner: Scanner,
    tracked: HashMap<GameId, Tracked>,
    launching: HashSet<GameId>,
    known: Option<HashSet<GameId>>,
    watcher: Option<Sender<LaunchEvent>>,
    monitor_running: bool,
    last_heartbeat: Instant,
}

struct Inner {
    sink: Arc<dyn SessionSink>,
    settings: RwLock<Arc<LaunchSettings>>,
    state: Mutex<State>,
    overrides: Mutex<HashMap<Store, Arc<dyn LaunchStrategy>>>,
    steam_started_by_us: Arc<AtomicBool>,
    updater: Mutex<Option<UpdaterRun>>,
    /// When each runtime's background update was last tried (failures are retried hourly, not every poll).
    update_tries: Mutex<HashMap<&'static str, Instant>>,
}

/// What a poll decided; delivered to the UI after the lock is released.
enum Outcome {
    Running { game: GameId, started_at: i64, to: Option<Sender<LaunchEvent>> },
    Exited { game: GameId, ended_at: i64, to: Option<Sender<LaunchEvent>> },
    /// New processes of a running game to register with GameMode.
    GameMode { game: GameId, bin: PathBuf, pids: Vec<u32> },
}

#[derive(Clone)]
pub struct Launcher {
    inner: Arc<Inner>,
}

impl Launcher {
    pub fn new(db: Db, settings: LaunchSettings) -> Launcher {
        Launcher::with_sink(Arc::new(db), settings)
    }

    pub fn with_sink(sink: Arc<dyn SessionSink>, settings: LaunchSettings) -> Launcher {
        let fs = settings.proc.clone();
        Launcher {
            inner: Arc::new(Inner {
                sink,
                settings: RwLock::new(Arc::new(settings)),
                state: Mutex::new(State {
                    scanner: Scanner::new(fs),
                    tracked: HashMap::new(),
                    launching: HashSet::new(),
                    known: None,
                    watcher: None,
                    monitor_running: false,
                    last_heartbeat: Instant::now(),
                }),
                overrides: Mutex::new(HashMap::new()),
                steam_started_by_us: Arc::new(AtomicBool::new(false)),
                updater: Mutex::new(None),
                update_tries: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn settings(&self) -> Arc<LaunchSettings> {
        self.inner.settings.read().unwrap().clone()
    }

    pub fn update_settings(&self, settings: LaunchSettings) {
        *self.inner.settings.write().unwrap() = Arc::new(settings);
    }

    /// Replace the strategy used for a store (tests, or a future lighter Steam strategy).
    pub fn set_strategy(&self, store: Store, strategy: Arc<dyn LaunchStrategy>) {
        self.inner.overrides.lock().unwrap().insert(store, strategy);
    }

    /// Games Grapple knows about. Detection of games started elsewhere only reports these (Steam runs many
    /// non-game apps). Call after every scan.
    pub fn set_known_games(&self, games: &[Game]) {
        let owners = if cfg!(windows) { winproc::owners(games, &self.settings().manual, &winproc::windows_dir()) } else { Vec::new() };
        let mut st = self.state();
        st.known = Some(games.iter().map(|g| g.id.clone()).collect());
        // Windows: processes are tagged by their executable's folder, so new owners mean new tags.
        if st.scanner.fs().set_owners(owners) {
            st.scanner.forget();
        }
    }

    /// Games running now with the unix time they started.
    pub fn running(&self) -> Vec<(GameId, i64)> {
        let mut v: Vec<_> = self.state().tracked.iter().map(|(id, t)| (id.clone(), t.started_at)).collect();
        v.sort();
        v
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn strategy_for(&self, store: Store) -> Arc<dyn LaunchStrategy> {
        if let Some(s) = self.inner.overrides.lock().unwrap().get(&store) {
            return s.clone();
        }
        let settings = self.settings();
        match store {
            Store::Steam => Arc::new(steam::SteamStrategy::new(settings.steam.clone())),
            Store::Epic => Arc::new(epic::EpicStrategy::new((*settings).clone())),
            Store::Manual => Arc::new(manual::ManualStrategy::new((*settings).clone())),
        }
    }

    // ---------------------------------------------------------------- launching

    /// Start `game`. Returns once its process is running (the `Running` event has been sent) or the launch failed
    /// (a `Failed` event has been sent). Tracking continues in the background until the game exits.
    pub async fn launch(&self, game: &Game, events: Sender<LaunchEvent>) -> Result<()> {
        let already = {
            let mut st = self.state();
            match st.tracked.get(&game.id).map(|t| t.started_at) {
                Some(started_at) => Some(started_at),
                None => {
                    if !st.launching.insert(game.id.clone()) {
                        return Ok(());
                    }
                    None
                }
            }
        };
        if let Some(started_at) = already {
            let _ = events.send(LaunchEvent::Running { game: game.id.clone(), started_at }).await;
            return Ok(());
        }
        self.yield_runtime_update(&events).await;
        let result = self.launch_inner(game, &events).await;
        self.state().launching.remove(&game.id);
        if let Err(e) = &result {
            let _ = events.send(LaunchEvent::Failed { game: game.id.clone(), message: e.to_string() }).await;
        }
        result
    }

    async fn launch_inner(&self, game: &Game, events: &Sender<LaunchEvent>) -> Result<()> {
        let settings = self.settings();
        let strategy = self.strategy_for(game.store);
        let ctx = StartCtx { events: events.clone(), proc: settings.proc.clone(), steam_started_by_us: self.inner.steam_started_by_us.clone(), log_dir: settings.log_dir.clone() };
        let started = strategy.start(game, &ctx).await?;

        // Wait for the game's own processes.
        let t0 = Instant::now();
        let matcher = started.matcher.clone();
        let mut members: HashSet<u32> = HashSet::new();
        let mut first = true;
        // umu's output (only during the launch): a runtime download is a step and not the game's start-up time.
        let mut tail = started.log.as_ref().filter(|_| started.umu_log).map(umu::LogTail::new);
        let mut runtime = umu::RuntimeWatch::default();
        // `appear_timeout` counts from the start, or from the end of a runtime download.
        let mut waiting_since = t0;
        loop {
            // Exit first, then the output: whatever umu wrote before it exited is read below.
            let exited = started.child_exit.as_ref().and_then(|e| *e.borrow());
            if let Some(t) = tail.as_mut() {
                for line in t.lines() {
                    let was_busy = runtime.busy();
                    if let Some(done) = runtime.feed(&line) {
                        let _ = events.send(LaunchEvent::Step { label: STEP_RUNTIME.to_string(), done }).await;
                    }
                    if was_busy && !runtime.busy() {
                        waiting_since = Instant::now();
                    }
                }
            }
            let alive = {
                let mut st = self.state();
                let delta = st.scanner.refresh();
                matcher.prune(&st.scanner, &mut members);
                if first {
                    let all: Vec<u32> = st.scanner.all().map(|p| p.pid).collect();
                    matcher.grow(&st.scanner, &mut members, &all);
                    first = false;
                } else {
                    matcher.grow(&st.scanner, &mut members, &delta.new);
                }
                !significant(&st.scanner, &members).is_empty()
            };
            let age = t0.elapsed();
            if let Some(code) = exited {
                // umu gave up on the runtime (or died while fetching it): say so, not "exited early".
                if runtime.busy() || (runtime.failed && !runtime.ready()) {
                    tracing::warn!("{}: umu could not download its runtime (exit code {code})", game.name);
                    return Err(LaunchError::RuntimeDownload);
                }
                if age < started.min_alive.max(Duration::from_secs(1)) || !alive {
                    let log = started.log.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    return Err(LaunchError::ExitedEarly { code: code.to_string(), log });
                }
            }
            if runtime.busy() {
                // umu itself is alive and downloading: not the game yet, and not a timeout.
                if age > RUNTIME_DOWNLOAD_LIMIT {
                    return Err(LaunchError::RuntimeDownload);
                }
                tokio::time::sleep(LAUNCH_POLL).await;
                continue;
            }
            // Through umu, the game is starting once umu is past its runtime setup.
            let settled = tail.is_none() || runtime.ready() || age >= UMU_QUIET;
            if alive && age >= started.min_alive && settled {
                break;
            }
            if waiting_since.elapsed() > started.appear_timeout {
                return Err(LaunchError::Timeout(format!("{} did not start in time", game.name)));
            }
            tokio::time::sleep(LAUNCH_POLL).await;
        }

        let started_at = paths::now();
        let session_id = self.inner.sink.start_session(&game.id, started_at, SessionSource::Grapple)?;
        let mut ask = None;
        {
            let mut st = self.state();
            let gm = started.gamemode.clone().map(|req| {
                let all: Vec<u32> = members.iter().copied().collect();
                let pids = gamemode::targets(&st.scanner, &all, &members, req.windows, &HashSet::new());
                ask = Some((req.bin.clone(), pids.clone()));
                GameModeTrack { req, done: pids.into_iter().collect() }
            });
            st.tracked.insert(
                game.id.clone(),
                Tracked {
                    session_id,
                    started_at,
                    matcher: started.matcher.clone(),
                    members,
                    missing: 0,
                    last_alive: started_at,
                    events: Some(events.clone()),
                    gamemode: gm,
                },
            );
        }
        if let Some((bin, pids)) = ask {
            ask_gamemode(game.id.clone(), bin, pids);
        }
        self.heartbeat_now();
        let _ = events.send(LaunchEvent::Step { label: started.final_step.to_string(), done: true }).await;
        let _ = events.send(LaunchEvent::Running { game: game.id.clone(), started_at }).await;
        self.ensure_monitor();
        Ok(())
    }

    // ---------------------------------------------------------------- stopping

    /// Ask the game to quit: SIGTERM to its processes (the whole tree for wine games). Never signals Steam
    /// itself and never sends SIGKILL.
    pub async fn stop(&self, game: &GameId) -> Result<()> {
        let this = self.clone();
        let game = game.clone();
        tokio::task::spawn_blocking(move || this.stop_blocking(&game))
            .await
            .map_err(|e| LaunchError::Config(e.to_string()))?
    }

    fn stop_blocking(&self, game: &GameId) -> Result<()> {
        let settings = self.settings();
        let mut guard = self.state();
        let st = &mut *guard;
        let delta = st.scanner.refresh();
        let members: HashSet<u32>;
        let mut never: HashSet<u32> = HashSet::new();
        if let Some(pid) = steam_pid(st.scanner.fs(), &settings.steam.pid_file) {
            never.insert(pid);
        }
        match st.tracked.get_mut(game) {
            Some(t) => {
                t.matcher.grow(&st.scanner, &mut t.members, &delta.new);
                members = t.members.clone();
            }
            None => {
                members = Matcher::new(game.clone()).find_all(&st.scanner);
            }
        }
        let sig = significant(&st.scanner, &members);
        if sig.is_empty() {
            return Err(LaunchError::NotRunning(game.0.clone()));
        }
        // Everything of the game, helpers included: wineserver and friends should go down with it.
        terminate(&st.scanner, &members, &never);
        Ok(())
    }

    // ---------------------------------------------------------------- watching

    /// Start the background watcher: closes sessions a crash left open, adopts games that are still running,
    /// notices games started outside Grapple (source `Detected`), and starts Steam hidden if "Keep Steam ready" is on.
    /// Running/Exited events of detected games go to `events`. Works with or without a tokio runtime on the
    /// calling thread.
    pub fn spawn_watcher(&self, events: Sender<LaunchEvent>) {
        self.state().watcher = Some(events.clone());
        let this = self.clone();
        let fut = async move {
            // Find out once, off any launch, whether children can get their own systemd scope.
            let _ = tokio::task::spawn_blocking(spawn::scopes_available).await;
            if let Err(e) = this.recover(&events).await {
                tracing::warn!("recovering sessions failed: {e}");
            }
            let settings = this.settings();
            if settings.keep_steam_ready {
                match settings.steam.start_ready(&settings.proc, &this.inner.steam_started_by_us).await {
                    Ok(true) => tracing::info!("started Steam hidden (keep Steam ready)"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("could not start Steam: {e}"),
                }
            }
            this.ensure_monitor();
            let updates = this.clone();
            tokio::spawn(async move { updates.runtime_update_loop().await });
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn(fut);
            }
            Err(_) => {
                // Not inside a runtime (e.g. called from a plain setup hook): give the watcher its own thread.
                std::thread::Builder::new()
                    .name("grapple-watcher".into())
                    .spawn(move || {
                        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                            rt.block_on(async move {
                                fut.await;
                                // keep this runtime alive for the monitor loop spawned by ensure_monitor
                                std::future::pending::<()>().await;
                            });
                        }
                    })
                    .ok();
            }
        }
    }

    /// Close sessions that a crash left open, and re-adopt games that are still running.
    /// Public so the app can call it explicitly; `spawn_watcher` does it at startup.
    pub async fn recover(&self, events: &Sender<LaunchEvent>) -> Result<usize> {
        let this = self.clone();
        let outcomes = tokio::task::spawn_blocking(move || this.recover_blocking()).await.map_err(|e| LaunchError::Config(e.to_string()))??;
        let n = outcomes.len();
        if n > 0 {
            self.heartbeat_now();
        }
        for o in outcomes {
            deliver(o, Some(events)).await;
        }
        Ok(n)
    }

    fn recover_blocking(&self) -> Result<Vec<Outcome>> {
        let open = self.inner.sink.open_sessions()?;
        let mut out = Vec::new();
        let mut st = self.state();
        st.scanner.refresh();
        for s in open {
            if st.tracked.contains_key(&s.game) {
                continue;
            }
            let matcher = Matcher::new(s.game.clone());
            let members = matcher.find_all(&st.scanner);
            if !significant(&st.scanner, &members).is_empty() {
                // Still running: keep the open session and track it.
                st.tracked.insert(
                    s.game.clone(),
                    Tracked {
                        session_id: s.id,
                        started_at: s.started_at,
                        matcher,
                        members,
                        missing: 0,
                        last_alive: paths::now(),
                        events: None,
                        gamemode: None,
                    },
                );
                out.push(Outcome::Running { game: s.game.clone(), started_at: s.started_at, to: None });
            } else {
                let at = self.inner.sink.last_seen(s.id).ok().flatten().unwrap_or(s.started_at).max(s.started_at);
                self.inner.sink.end_session(s.id, at)?;
            }
        }
        Ok(out)
    }

    fn ensure_monitor(&self) {
        {
            let mut st = self.state();
            if st.monitor_running {
                return;
            }
            st.monitor_running = true;
        }
        let this = self.clone();
        let fut = async move { this.monitor_loop().await };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn(fut);
            }
            Err(_) => {
                self.state().monitor_running = false;
            }
        }
    }

    async fn monitor_loop(self) {
        loop {
            let (busy, idle) = {
                let st = self.state();
                (!st.tracked.is_empty(), self.settings().poll_interval)
            };
            tokio::time::sleep(if busy { TRACK_POLL.min(idle) } else { idle }).await;
            let this = self.clone();
            let polled = tokio::task::spawn_blocking(move || this.poll_once()).await;
            let (outcomes, keep_going) = match polled {
                Ok(v) => v,
                Err(_) => (Vec::new(), true),
            };
            let watcher = self.state().watcher.clone();
            for o in outcomes {
                if let Outcome::GameMode { game, bin, pids } = o {
                    ask_gamemode(game, bin, pids);
                    continue;
                }
                let exited = match &o {
                    Outcome::Exited { game, .. } => Some(game.clone()),
                    _ => None,
                };
                deliver(o, watcher.as_ref()).await;
                if let Some(game) = exited {
                    if store_of(&game) == Store::Steam {
                        let this = self.clone();
                        tokio::spawn(async move { this.maybe_close_steam(&game).await });
                    }
                }
            }
            if !keep_going {
                return;
            }
        }
    }

    /// One poll: refresh the process cache, end games that are gone, detect new ones. Returns the events to send
    /// and whether the monitor should keep running.
    fn poll_once(&self) -> (Vec<Outcome>, bool) {
        let mut outcomes = Vec::new();
        let now = paths::now();
        let mut st = self.state();
        st.scanner.refresh();
        let new_pids = st.scanner.take_pending();
        let st = &mut *st;

        // Tracked games: update members, count polls without a live process.
        let mut ended: Vec<GameId> = Vec::new();
        for (id, t) in st.tracked.iter_mut() {
            t.matcher.prune(&st.scanner, &mut t.members);
            t.matcher.grow(&st.scanner, &mut t.members, &new_pids);
            if let Some(gm) = t.gamemode.as_mut() {
                let pids = gamemode::targets(&st.scanner, &new_pids, &t.members, gm.req.windows, &gm.done);
                if !pids.is_empty() {
                    gm.done.extend(pids.iter().copied());
                    outcomes.push(Outcome::GameMode { game: id.clone(), bin: gm.req.bin.clone(), pids });
                }
            }
            if significant(&st.scanner, &t.members).is_empty() {
                t.missing += 1;
                if t.missing >= GRACE_POLLS {
                    ended.push(id.clone());
                }
            } else {
                t.missing = 0;
                t.last_alive = now;
            }
        }
        for id in ended {
            if let Some(t) = st.tracked.remove(&id) {
                if let Err(e) = self.inner.sink.end_session(t.session_id, t.last_alive) {
                    tracing::warn!("could not end session: {e}");
                }
                outcomes.push(Outcome::Exited { game: id, ended_at: t.last_alive, to: t.events });
            }
        }

        // Games started outside Grapple.
        if st.watcher.is_some() {
            for pid in &new_pids {
                let Some(p) = st.scanner.get(*pid) else { continue };
                if !p.alive() || proc::is_helper(&p.comm) || proc::is_protected(&p.comm) {
                    continue;
                }
                let mut candidates: Vec<GameId> = Vec::new();
                if let Some(id) = &p.tags.steam_app {
                    candidates.push(GameId::new(Store::Steam, id));
                }
                if let Some(app) = &p.tags.heroic_app {
                    candidates.push(GameId::new(Store::Epic, app));
                }
                if let Some(g) = &p.tags.grapple_game {
                    candidates.push(GameId(g.clone()));
                }
                for id in candidates {
                    if st.tracked.contains_key(&id) || st.launching.contains(&id) {
                        continue;
                    }
                    if st.known.as_ref().is_some_and(|k| !k.contains(&id)) {
                        continue;
                    }
                    let matcher = Matcher::new(id.clone());
                    let members = matcher.find_all(&st.scanner);
                    if significant(&st.scanner, &members).is_empty() {
                        continue;
                    }
                    match self.inner.sink.start_session(&id, now, SessionSource::Detected) {
                        Ok(session_id) => {
                            st.tracked.insert(
                                id.clone(),
                                Tracked { session_id, started_at: now, matcher, members, missing: 0, last_alive: now, events: None, gamemode: None },
                            );
                            outcomes.push(Outcome::Running { game: id, started_at: now, to: None });
                        }
                        Err(e) => tracing::warn!("could not record session: {e}"),
                    }
                }
            }
        }

        // Remember "still running" for crash recovery (at once when a game was just detected).
        let detected_now = outcomes.iter().any(|o| matches!(o, Outcome::Running { .. }));
        if (detected_now || st.last_heartbeat.elapsed() >= HEARTBEAT_EVERY) && !st.tracked.is_empty() {
            let seen: Vec<(i64, i64)> = st.tracked.values().map(|t| (t.session_id, now)).collect();
            let _ = self.inner.sink.heartbeat(&seen);
            st.last_heartbeat = Instant::now();
        }

        let keep = !st.tracked.is_empty() || st.watcher.is_some();
        if !keep {
            st.monitor_running = false;
        }
        (outcomes, keep)
    }

    /// Save "these sessions are running now" so a crash can close them at the right time.
    fn heartbeat_now(&self) {
        let mut st = self.state();
        let now = paths::now();
        let seen: Vec<(i64, i64)> = st.tracked.values().map(|t| (t.session_id, now)).collect();
        if let Err(e) = self.inner.sink.heartbeat(&seen) {
            tracing::warn!("could not save heartbeat: {e}");
        }
        st.last_heartbeat = Instant::now();
    }

    // ---------------------------------------------------------------- umu's runtime

    /// Is a game running or starting (ours, another launcher's, or any Proton / Steam Linux Runtime
    /// container)? Then the runtime is not touched.
    fn games_busy(&self) -> bool {
        let mut st = self.state();
        if !st.tracked.is_empty() || !st.launching.is_empty() {
            return true;
        }
        st.scanner.refresh();
        st.scanner.all().any(|p| {
            p.alive()
                && (p.tags.steam_app.is_some()
                    || p.tags.heroic_app.is_some()
                    || p.tags.grapple_game.is_some()
                    || umu::is_container_process(&p.comm))
        })
    }

    fn updater(s: &LaunchSettings) -> Option<umu::Updater> {
        if cfg!(windows) {
            // umu and its Steam Linux Runtime are Linux-only; Windows games run natively.
            return None;
        }
        let umu_run = heroic::find_umu(s.umu_run.as_deref(), &s.heroic_dir, &env::child_path(), false)?;
        Some(umu::Updater { umu_run, umu_dir: s.umu_dir.clone(), log_dir: s.log_dir.clone() })
    }

    async fn runtime_update_loop(self) {
        tokio::time::sleep(RUNTIME_UPDATE_START).await;
        loop {
            self.update_runtimes_if_due().await;
            tokio::time::sleep(RUNTIME_UPDATE_POLL).await;
        }
    }

    /// One round of background updates: every runtime umu has set up whose check is due, while nothing runs.
    pub async fn update_runtimes_if_due(&self) {
        let settings = self.settings();
        if !settings.runtime_updates || umu::updates_disabled_by_user() {
            return;
        }
        let Some(up) = Self::updater(&settings) else { return };
        for rt in up.runtimes() {
            let last_try = self.inner.update_tries.lock().unwrap_or_else(|e| e.into_inner()).get(rt.variant).map(Instant::elapsed);
            if !up.due(rt, SystemTime::now(), last_try) {
                continue;
            }
            let this = self.clone();
            if tokio::task::spawn_blocking(move || this.games_busy()).await.unwrap_or(true) {
                return;
            }
            self.inner.update_tries.lock().unwrap_or_else(|e| e.into_inner()).insert(rt.variant, Instant::now());
            if let Err(e) = self.update_runtime_now(rt).await {
                tracing::warn!("could not update {}: {e}", rt.variant);
            }
        }
    }

    /// Run umu's updater for `rt` now and wait for it (the caller made sure nothing runs). `Ok(true)`: the
    /// runtime is current. A launch that starts meanwhile stops it if it is still downloading.
    pub async fn update_runtime_now(&self, rt: umu::Runtime) -> Result<bool> {
        let settings = self.settings();
        let up = Self::updater(&settings).ok_or(LaunchError::Missing { what: "umu-run", detail: "not installed".into() })?;
        let log = up.log(rt);
        let spawned = spawn::spawn_plan(&up.plan(rt), None, Some(&log), &settings.proc)?;
        tracing::info!("checking for a newer {} in the background (log: {})", rt.variant, log.display());
        *self.inner.updater.lock().unwrap_or_else(|e| e.into_inner()) = Some(UpdaterRun { pid: spawned.pid, log: log.clone(), exit: spawned.exit.clone() });
        let mut exit = spawned.exit;
        let code = exit.wait_for(Option::is_some).await.map(|c| c.unwrap_or(-1)).unwrap_or(-1);
        {
            let mut cur = self.inner.updater.lock().unwrap_or_else(|e| e.into_inner());
            if cur.as_ref().is_some_and(|r| r.pid == spawned.pid) {
                *cur = None;
            }
        }
        let ok = up.succeeded(rt, code);
        if ok {
            up.mark_checked(rt)?;
            tracing::info!("{} is current", rt.variant);
        } else {
            tracing::info!("{} was not updated this time (exit code {code}); see {}", rt.variant, log.display());
        }
        Ok(ok)
    }

    /// Before a launch: a background update still downloading is stopped (it starts over later); one that is
    /// already unpacking is waited for, so a runtime is never swapped under a starting game.
    async fn yield_runtime_update(&self, events: &Sender<LaunchEvent>) {
        let Some(run) = self.inner.updater.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let mut exit = run.exit.clone();
        if exit.borrow().is_some() {
            return;
        }
        let phase = umu::phase_of(&run.log).phase;
        if phase == umu::Phase::Installing {
            let _ = events.send(LaunchEvent::Step { label: STEP_RUNTIME.to_string(), done: false }).await;
            if tokio::time::timeout(RUNTIME_INSTALL_WAIT, exit.wait_for(Option::is_some)).await.is_err() {
                stop_group(run.pid);
            }
            let _ = events.send(LaunchEvent::Step { label: STEP_RUNTIME.to_string(), done: true }).await;
            return;
        }
        tracing::info!("a game is starting: stopping the background runtime update (log: {})", run.log.display());
        stop_group(run.pid);
        let _ = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some)).await;
    }

    // ---------------------------------------------------------------- Steam housekeeping

    fn any_steam_game_running(&self) -> bool {
        let mut st = self.state();
        st.scanner.refresh();
        let tracked_steam = st.tracked.keys().any(|id| store_of(id) == Store::Steam);
        tracked_steam
            || st.scanner.all().any(|p| p.tags.steam_app.is_some() && p.alive() && !proc::is_helper(&p.comm) && !proc::is_protected(&p.comm))
    }

    /// After a Steam game ended: `steam -shutdown` if the settings say so and no other Steam game is running.
    async fn maybe_close_steam(&self, game: &GameId) {
        let settings = self.settings();
        let by_us = self.inner.steam_started_by_us.load(Ordering::SeqCst);
        if !settings.should_close_steam(game, by_us) {
            return;
        }
        // Give Steam a moment to finish its own bookkeeping (cloud sync, playtime) before asking it to quit.
        tokio::time::sleep(settings.steam.shutdown_delay).await;
        let this = self.clone();
        let busy = tokio::task::spawn_blocking(move || this.any_steam_game_running()).await.unwrap_or(true);
        if busy || !settings.steam.is_running(&settings.proc) {
            return;
        }
        match settings.steam.shutdown(&settings.proc) {
            Ok(()) => {
                self.inner.steam_started_by_us.store(false, Ordering::SeqCst);
                tracing::info!("asked Steam to shut down");
            }
            Err(e) => tracing::warn!("could not shut Steam down: {e}"),
        }
    }

    /// Process cache (for the app to show e.g. "Steam is running"): is the Steam client up?
    pub fn steam_running(&self) -> bool {
        let s = self.settings();
        s.steam.is_running(&s.proc)
    }
}

/// SIGTERM the process group `pid` leads (everything Grapple spawns is a session leader, see [`spawn`]).
#[cfg(not(windows))]
fn stop_group(pid: u32) {
    if pid > 1 {
        // SAFETY: plain kill(2) on a process group Grapple created.
        unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
    }
}

/// Only the umu runtime updater is stopped this way, and it never runs on Windows ([`Launcher::updater`]).
#[cfg(windows)]
fn stop_group(_pid: u32) {}

/// Register `pids` of `game` with GameMode, off the async threads (each is a short `gamemoded` call).
fn ask_gamemode(game: GameId, bin: PathBuf, pids: Vec<u32>) {
    if pids.is_empty() {
        return;
    }
    let work = move || {
        for pid in pids {
            match gamemode::register(Path::new(&bin), pid) {
                Ok(()) => tracing::info!("GameMode on for {} (pid {pid})", game.0),
                Err(e) => tracing::warn!("GameMode request for {} (pid {pid}) failed: {e}", game.0),
            }
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn_blocking(work);
        }
        Err(_) => work(),
    }
}

async fn deliver(o: Outcome, watcher: Option<&Sender<LaunchEvent>>) {
    let (event, to) = match o {
        Outcome::Running { game, started_at, to } => (LaunchEvent::Running { game, started_at }, to),
        Outcome::Exited { game, ended_at, to } => (LaunchEvent::Exited { game, ended_at }, to),
        Outcome::GameMode { .. } => return,
    };
    // The launch card's channel first; if the UI dropped it (card closed), the watcher's channel.
    match to {
        Some(tx) if !tx.is_closed() => {
            let _ = tx.send(event).await;
        }
        _ => {
            if let Some(w) = watcher {
                let _ = w.send(event).await;
            }
        }
    }
}

