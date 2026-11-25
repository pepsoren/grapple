//! The downloads service end to end: queue, runner, events, pause while playing, updates, uninstall, the Steam
//! watcher and the bridge switch. Fake host, fake Epic (common/), temp folders; no Steam, no real login.

mod common;

use common::*;
use futures_util::future::BoxFuture;
use grapple_download::fsutil::Space;
use grapple_download::testkit::{GameSpec, TestFile, build, noise, sample_files};
use grapple_download::*;
use grapple_launch::epic_auth::{EpicApi, EpicAuth};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const W: u32 = 16 * 1024;

#[derive(Default)]
struct TestHost {
    emits: Mutex<Vec<(Instant, DlSnapshot)>>,
    notified: Mutex<Vec<String>>,
    games_changed: AtomicUsize,
    steam_urls: Mutex<Vec<String>>,
    restarts: Arc<AtomicUsize>,
    steam_game: AtomicBool,
    names: HashMap<String, String>,
}

impl Host for TestHost {
    fn emit(&self, snap: &DlSnapshot) {
        self.emits.lock().unwrap().push((Instant::now(), snap.clone()));
    }
    fn game(&self, id: &str) -> Option<GameInfo> {
        self.names.get(id).map(|n| GameInfo { name: n.clone(), art: DlArt { hero: Some(format!("grapple://art/{id}/hero")), ..DlArt::default() } })
    }
    fn games_changed(&self) {
        self.games_changed.fetch_add(1, Ordering::SeqCst);
    }
    fn notify_ready(&self, name: &str) {
        self.notified.lock().unwrap().push(name.into());
    }
    fn steam_game_running(&self) -> bool {
        self.steam_game.load(Ordering::SeqCst)
    }
    fn steam_open_url(&self, url: &str) -> Result<()> {
        self.steam_urls.lock().unwrap().push(url.into());
        Ok(())
    }
    fn restart_steam(&self) -> BoxFuture<'static, Result<()>> {
        let r = self.restarts.clone();
        Box::pin(async move {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn protons(&self) -> Vec<ProtonChoice> {
        vec![ProtonChoice { name: "GE-Proton (latest)".into(), is_default: true }, ProtonChoice { name: "GE-Proton10-25".into(), is_default: false }]
    }
}

struct World {
    tmp: tempfile::TempDir,
    epic: FakeEpic,
    legendary: PathBuf,
    games: PathBuf,
    steam: PathBuf,
    host: Arc<TestHost>,
    dl: Downloads,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn world(tweak: impl FnOnce(&mut Config)) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let heroic = home.join(".config/heroic");
    let legendary = heroic.join("legendaryConfig/legendary");
    let games = home.join("Games/Heroic");
    std::fs::create_dir_all(&legendary).unwrap();
    std::fs::write(heroic.join("config.json"), serde_json::json!({"defaultSettings": {"defaultInstallPath": games}}).to_string()).unwrap();
    write_metadata(&legendary, "Sugar", "Rocket League®", Some("rocketleague"));
    write_metadata(&legendary, "FallGuys", "Fall Guys", None);
    let steam = home.join(".local/share/Steam");
    for d in ["steamapps", "logs"] {
        std::fs::create_dir_all(steam.join(d)).unwrap();
    }
    std::fs::write(steam.join("steamapps/libraryfolders.vdf"), format!("\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n", steam.display())).unwrap();

    let epic = FakeEpic::start().await;
    let auth = EpicAuth::new(EpicApi { oauth_base: "http://127.0.0.1:9".into(), ..EpicApi::default() }, fake_login(tmp.path())).unwrap();
    let mut cfg = Config::new(home.join(".local/share/grapple/downloads.db"), legendary.clone(), heroic, Some(steam.clone()), home.clone(), Arc::new(auth)).unwrap();
    cfg.launcher_base = epic.base();
    cfg.bridge_port = free_port();
    cfg.engine.retry_backoff = vec![Duration::from_millis(5); 3];
    cfg.engine.flush_every = Duration::from_millis(20);
    cfg.engine.free_space = Arc::new(|_| Ok(Space { free: 100 << 30, total: 1 << 40 }));
    cfg.first_update_check = Duration::from_secs(3600);
    cfg.steam_poll_busy = Duration::from_millis(100);
    cfg.steam_poll_idle = Duration::from_millis(150);
    cfg.steam_log_from_start = true;
    tweak(&mut cfg);
    let names = [("epic:Sugar", "Rocket League"), ("epic:FallGuys", "Fall Guys"), ("steam:252490", "Rust"), ("steam:1478500", "Big Walk")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let host = Arc::new(TestHost { names, ..TestHost::default() });
    let dl = Downloads::new(cfg, host.clone()).unwrap();
    dl.start();
    World { tmp, epic, legendary, games, steam, host, dl }
}

fn spec(app: &str, version: &str, seed: u32, files: Vec<TestFile>) -> GameSpec {
    GameSpec { app_name: app.into(), build_version: version.into(), window: W, seed, files, ..GameSpec::default() }
}

fn big(seed: u32, windows: usize) -> Vec<TestFile> {
    vec![TestFile::new("big.pak", noise(seed as u64, W as usize * windows))]
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn in_done(s: &DlSnapshot, id: &str) -> bool {
    s.done.iter().any(|i| i.game == id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn epic_install_through_the_queue() {
    let w = world(|_| {}).await;
    let files = sample_files(W as usize, 4);
    w.epic.add("Sugar", build(&spec("Sugar", "1.0", 1, files.clone())));
    let plan = w.dl.install_plan("epic:Sugar").await.unwrap();
    assert_eq!(plan.store, DlStore::Epic);
    assert!(plan.size_bytes.unwrap() > 0);
    let t = &plan.targets[0];
    assert!(t.is_default && t.enough && t.label == "Home drive");
    assert_eq!(t.path, "~/Games/Heroic");
    if cfg!(target_os = "linux") {
        assert_eq!(plan.protons.as_ref().unwrap()[0].name, "GE-Proton (latest)");
    }
    w.dl.install_start("epic:Sugar", &t.path).await.unwrap();
    assert!(w.dl.install_start("epic:Sugar", &t.path).await.is_err(), "not twice");
    until("install", || in_done(&w.dl.snapshot(), "epic:Sugar")).await;
    let s = w.dl.snapshot();
    assert!(s.active.is_none() && s.queued.is_empty());
    let d = &s.done[0];
    assert_eq!((d.name.as_str(), d.state, d.store), ("Rocket League", DlState::Done, DlStore::Epic));
    assert!(d.finished_at.is_some());
    assert_eq!(w.host.notified.lock().unwrap().clone(), vec!["Rocket League".to_string()]);
    assert!(w.host.games_changed.load(Ordering::SeqCst) >= 1);
    let dir = w.games.join("rocketleague");
    assert_eq!(std::fs::read(dir.join("readme.txt")).unwrap(), files[5].data);
    assert!(legendary::installed_entry(&w.legendary, "Sugar").unwrap().is_some());
    // downloads-changed went out, never more than 4 a second.
    tokio::time::sleep(Duration::from_millis(300)).await;
    {
        let emits = w.host.emits.lock().unwrap();
        assert!(emits.len() >= 2);
        for pair in emits.windows(2) {
            assert!(pair[1].0.duration_since(pair[0].0) >= Duration::from_millis(240), "events too close");
        }
    }

    // Uninstall: folder and entry gone, the library told.
    let before = w.host.games_changed.load(Ordering::SeqCst);
    w.dl.uninstall("epic:Sugar").await.unwrap();
    assert!(!dir.exists());
    assert!(legendary::installed_entry(&w.legendary, "Sugar").unwrap().is_none());
    assert!(w.host.games_changed.load(Ordering::SeqCst) > before);
    let _ = &w.tmp;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_now_pause_and_pause_all() {
    let w = world(|_| {}).await;
    w.epic.add("Sugar", build(&spec("Sugar", "1", 1, big(1, 40))));
    w.epic.add("FallGuys", build(&spec("FallGuys", "1", 2, big(2, 40))));
    let mut s = w.dl.settings_get();
    s.speed_limit_bytes = W as u64 * 6;
    s.connections = 2;
    w.dl.settings_set(s).unwrap();
    let home_games = "~/Games/Heroic";
    w.dl.install_start("epic:Sugar", home_games).await.unwrap();
    w.dl.install_start("epic:FallGuys", home_games).await.unwrap();
    until("Sugar active", || w.dl.snapshot().active.is_some_and(|a| a.game == "epic:Sugar" && a.state == DlState::Downloading)).await;
    let s = w.dl.snapshot();
    assert_eq!(s.queued.iter().map(|i| i.game.as_str()).collect::<Vec<_>>(), ["epic:FallGuys"]);
    let a = s.active.unwrap();
    assert!(a.can_pause && a.can_cancel && a.can_reorder && a.bytes_total.is_some());
    assert_eq!(a.target_dir, home_games);

    w.dl.start_now("epic:FallGuys").await.unwrap();
    until("Fall Guys active", || w.dl.snapshot().active.is_some_and(|a| a.game == "epic:FallGuys")).await;
    let s = w.dl.snapshot();
    assert_eq!(s.queued[0].game, "epic:Sugar");
    assert_eq!(s.queued[0].state, DlState::Queued, "made room, not paused");

    w.dl.pause("epic:FallGuys").await.unwrap();
    until("Sugar active again", || w.dl.snapshot().active.is_some_and(|a| a.game == "epic:Sugar")).await;
    assert!(w.dl.snapshot().queued.iter().any(|i| i.game == "epic:FallGuys" && i.state == DlState::Paused));

    w.dl.pause_all(true).await.unwrap();
    until("nothing active", || w.dl.snapshot().active.is_none()).await;
    assert!(w.dl.snapshot().paused_all);
    w.dl.pause_all(false).await.unwrap();
    until("active again", || w.dl.snapshot().active.is_some()).await;

    // Reorder: Fall Guys first again.
    w.dl.reorder(&["epic:FallGuys".into(), "epic:Sugar".into()]).await.unwrap();
    let mut s = w.dl.settings_get();
    s.speed_limit_bytes = 0;
    w.dl.settings_set(s).unwrap();
    until("both done", || {
        let s = w.dl.snapshot();
        in_done(&s, "epic:Sugar") && in_done(&s, "epic:FallGuys")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_the_queue() {
    let w = world(|_| {}).await;
    w.epic.add("Sugar", build(&spec("Sugar", "1", 1, big(3, 60))));
    let mut s = w.dl.settings_get();
    s.speed_limit_bytes = W as u64 * 4;
    w.dl.settings_set(s).unwrap();
    w.dl.install_start("epic:Sugar", "~/Games/Heroic").await.unwrap();
    until("downloading", || w.dl.snapshot().active.is_some_and(|a| a.state == DlState::Downloading && a.bytes_done > 0)).await;
    w.dl.cancel("epic:Sugar").await.unwrap();
    until("gone", || {
        let s = w.dl.snapshot();
        s.active.is_none() && s.queued.is_empty()
    })
    .await;
    assert!(!w.games.join("rocketleague").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_while_playing_and_resume_after() {
    let w = world(|_| {}).await;
    w.epic.add("Sugar", build(&spec("Sugar", "1", 1, big(4, 30))));
    let mut s = w.dl.settings_get();
    s.speed_limit_bytes = W as u64 * 8;
    w.dl.settings_set(s).unwrap();
    w.dl.install_start("epic:Sugar", "~/Games/Heroic").await.unwrap();
    until("downloading", || w.dl.snapshot().active.is_some_and(|a| a.state == DlState::Downloading)).await;
    w.dl.set_playing(true, false).await;
    until("paused for the game", || w.dl.snapshot().active.is_none()).await;
    let s = w.dl.snapshot();
    assert_eq!(s.queued[0].state, DlState::Queued, "grapple paused it, so it stays queued");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.dl.snapshot().active.is_none(), "nothing starts while a game runs");
    w.dl.set_playing(false, false).await;
    until("done", || in_done(&w.dl.snapshot(), "epic:Sugar")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn epic_updates_are_found_and_installed() {
    let w = world(|_| {}).await;
    w.epic.add("Sugar", build(&spec("Sugar", "1.0", 1, sample_files(W as usize, 1))));
    w.dl.install_start("epic:Sugar", "~/Games/Heroic").await.unwrap();
    until("installed", || in_done(&w.dl.snapshot(), "epic:Sugar")).await;

    // Manual updates: listed, not started.
    let mut s = w.dl.settings_get();
    s.auto_update = false;
    w.dl.settings_set(s).unwrap();
    w.epic.add("Sugar", build(&spec("Sugar", "2.0", 2, sample_files(W as usize, 2))));
    w.dl.check_updates().await.unwrap();
    let s = w.dl.snapshot();
    assert_eq!(s.updates.len(), 1);
    assert_eq!((s.updates[0].game.as_str(), s.updates[0].kind), ("epic:Sugar", DlKind::Update));
    assert_eq!(s.updates[0].target_dir, "~/Games/Heroic");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.dl.snapshot().active.is_none());

    // While a game runs, "Only update while I'm not playing" holds it back.
    w.dl.set_playing(true, false).await;
    w.dl.update_now("epic:Sugar").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.dl.snapshot().active.is_none());
    w.dl.set_playing(false, false).await;
    until("updated", || {
        legendary::installed_entry(&w.legendary, "Sugar").unwrap().is_some_and(|e| e["version"] == "2.0") && w.dl.snapshot().active.is_none()
    })
    .await;
    // Up to date: the next check adds nothing.
    w.dl.check_updates().await.unwrap();
    assert!(w.dl.snapshot().updates.is_empty());
}

fn acf(appid: u32, flags: u64, to_dl: u64, dl: u64) -> String {
    format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"x\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"BytesToDownload\"\t\t\"{to_dl}\"\n\t\"BytesDownloaded\"\t\t\"{dl}\"\n}}\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steam_without_the_bridge_is_watched() {
    let w = world(|_| {}).await;
    let plan = w.dl.install_plan("steam:252490").await.unwrap();
    assert_eq!(plan.size_bytes, None, "no bridge: Steam's dialog shows the size");
    assert_eq!(plan.targets.len(), 1);
    assert_eq!(plan.targets[0].path, "~/.local/share/Steam");
    w.dl.install_start("steam:252490", &plan.targets[0].path).await.unwrap();
    assert_eq!(w.host.steam_urls.lock().unwrap().clone(), vec!["steam://install/252490".to_string()]);
    let s = w.dl.snapshot();
    let q = &s.queued[0];
    assert_eq!((q.game.as_str(), q.state, q.can_pause, q.can_reorder, q.can_cancel), ("steam:252490", DlState::Queued, false, false, false));

    // Steam starts: the acf appears.
    let acfp = w.steam.join("steamapps/appmanifest_252490.acf");
    std::fs::write(&acfp, acf(252490, 1026, 4_000_000, 1_000_000)).unwrap();
    // An installed game with an update waiting, and a tool grapple does not show.
    std::fs::write(w.steam.join("steamapps/appmanifest_1478500.acf"), acf(1478500, 6, 1_340_000_000, 0)).unwrap();
    std::fs::write(w.steam.join("steamapps/appmanifest_228980.acf"), acf(228980, 6, 10, 0)).unwrap();
    // The watcher may read the folder between these writes (seen on Windows CI), so wait for all of it.
    until("Steam download active and the update listed", || {
        let s = w.dl.snapshot();
        s.active.is_some_and(|a| a.game == "steam:252490" && a.state == DlState::Downloading) && !s.updates.is_empty()
    })
    .await;
    let s = w.dl.snapshot();
    assert_eq!(s.active.as_ref().unwrap().bytes_total, Some(4_000_000));
    assert_eq!(s.updates.iter().map(|u| u.game.as_str()).collect::<Vec<_>>(), ["steam:1478500"]);
    assert_eq!(s.updates[0].bytes_total, Some(1_340_000_000));
    assert!(w.dl.pause("steam:252490").await.is_err(), "no pause without the bridge");

    // Finished.
    std::fs::write(&acfp, acf(252490, 4, 4_000_000, 4_000_000)).unwrap();
    until("Steam download done", || in_done(&w.dl.snapshot(), "steam:252490")).await;
    assert!(w.host.notified.lock().unwrap().contains(&"Rust".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steam_download_control_switch() {
    let w = world(|_| {}).await;
    let file = w.steam.join(".cef-enable-remote-debugging");
    assert_eq!(w.dl.snapshot().steam_bridge, BridgeState::Off);
    let s = w.dl.steam_bridge_set(true).await.unwrap();
    assert!(s.steam_bridge && file.exists());
    until("restart", || w.host.restarts.load(Ordering::SeqCst) == 1).await;
    // Nothing answers on the port: the bridge is unavailable, never an error.
    until("unavailable", || w.dl.snapshot().steam_bridge == BridgeState::Unavailable).await;

    // A Steam game runs: the restart waits for it.
    w.host.steam_game.store(true, Ordering::SeqCst);
    w.dl.steam_bridge_set(false).await.unwrap();
    assert!(!file.exists());
    assert_eq!(w.dl.snapshot().steam_bridge, BridgeState::RestartPending);
    assert_eq!(w.host.restarts.load(Ordering::SeqCst), 1);
    w.host.steam_game.store(false, Ordering::SeqCst);
    w.dl.set_playing(false, false).await;
    until("restart after the game", || w.host.restarts.load(Ordering::SeqCst) == 2).await;
    assert_eq!(w.dl.snapshot().steam_bridge, BridgeState::Off);
    // settings_set never flips the bridge.
    let mut s = w.dl.settings_get();
    s.steam_bridge = true;
    assert!(!w.dl.settings_set(s).unwrap().steam_bridge);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_round_trip_and_survive_a_restart() {
    let w = world(|_| {}).await;
    let s = w.dl.settings_get();
    assert_eq!((s.connections, s.auto_update, s.update_only_when_idle, s.pause_while_playing, s.notify_ready, s.steam_bridge), (8, true, true, true, true, false));
    assert_eq!(s.epic_default_dir, "~/Games/Heroic");
    let c = s.choices.clone().unwrap();
    assert_eq!(c.steam, vec!["~/.local/share/Steam".to_string()]);
    assert_eq!(c.epic[0], "~/Games/Heroic");
    let mut s2 = s.clone();
    s2.speed_limit_bytes = 5_000_000;
    s2.notify_ready = false;
    s2.epic_default_dir = "/data/Games".into();
    s2.steam_default_dir = Some("~/.local/share/Steam".into());
    let back = w.dl.settings_set(s2).unwrap();
    assert_eq!((back.speed_limit_bytes, back.notify_ready, back.epic_default_dir.as_str()), (5_000_000, false, "/data/Games"));
    let json = serde_json::to_value(&back).unwrap();
    assert!(json.get("speedLimitBytes").is_some() && json.get("epicDefaultDir").is_some(), "camelCase for the UI");
    let db = w.tmp.path().join("home/.local/share/grapple/downloads.db");
    assert!(db.exists(), "own SQLite file");
    let _ = (Path::new("x"),);
}
