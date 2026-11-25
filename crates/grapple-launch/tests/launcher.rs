//! End-to-end tests with real (harmless) processes: `sleep`, shell scripts, a fake `steam`. No game, no real Steam,
//! no network beyond a local mock, nothing under ~/.config.

// Linux (and other Unix) only: real processes from shell scripts and /proc.
#![cfg(unix)]

mod common;

use common::*;
use grapple_core::{Art, Game, GameId, LaunchEvent, SessionSource, Store};
use grapple_launch::epic_auth::EpicApi;
use grapple_launch::manual::{ManualEntry, ManualKind};
use grapple_launch::steam::{SteamSettings, SteamStrategy};
use grapple_launch::{LaunchError, LaunchSettings, Launcher, MemorySink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Tests that start processes carrying a SteamAppId share /proc, and "is any Steam game running?" looks at all of
/// it, so they take turns.
static STEAM_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn game(store: Store, id: &str) -> Game {
    Game {
        id: GameId::new(store, id),
        store,
        name: id.to_string(),
        sort_name: id.to_string(),
        installed: true,
        install_dir: None,
        size_bytes: None,
        art: Art::default(),
        store_playtime_min: 0,
        last_played: None,
        status: None,
    }
}

fn base_settings(dir: &Path) -> LaunchSettings {
    LaunchSettings {
        heroic_dir: dir.join("heroic"),
        legendary_dir: dir.join("legendary"),
        log_dir: dir.join("logs"),
        // never the real ~/.local/share/umu
        umu_dir: dir.join("umu"),
        poll_interval: Duration::from_millis(100),
        // a `steam` that cannot exist: no test may ever reach the real Steam
        steam: SteamSettings {
            bin: "/nonexistent/steam".into(),
            pid_file: dir.join("steam.pid"),
            shutdown_delay: Duration::from_millis(200),
            ..SteamSettings::default()
        },
        ..LaunchSettings::default()
    }
}

async fn next_matching(rx: &mut mpsc::Receiver<LaunchEvent>, secs: u64, f: impl Fn(&LaunchEvent) -> bool) -> LaunchEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(e)) if f(&e) => return e,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("channel closed"),
            Err(_) => panic!("timed out waiting for event"),
        }
    }
}

fn drain(rx: &mut mpsc::Receiver<LaunchEvent>) -> Vec<LaunchEvent> {
    let mut v = Vec::new();
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}

fn step(label: &str, done: bool) -> LaunchEvent {
    LaunchEvent::Step { label: label.into(), done }
}

fn sleep_bin() -> String {
    grapple_launch::paths::which("sleep").unwrap().to_string_lossy().into_owned()
}

#[tokio::test]
async fn manual_native_game_runs_is_tracked_and_stops_politely() {
    let dir = tempfile::tempdir().unwrap();
    let id = "t-native";
    let mut settings = base_settings(dir.path());
    settings.manual.insert(
        GameId::new(Store::Manual, id),
        ManualEntry { exe: sleep_bin(), args: vec!["30".into()], kind: ManualKind::Native, ..Default::default() },
    );
    let sink = Arc::new(MemorySink::new());
    let launcher = Launcher::with_sink(sink.clone(), settings);
    let g = game(Store::Manual, id);
    let (tx, mut rx) = mpsc::channel(64);

    launcher.launch(&g, tx).await.unwrap();
    let events = drain(&mut rx);
    assert_eq!(events[0], step("Starting game", false));
    assert_eq!(events[1], step("Starting game", true));
    assert!(matches!(&events[2], LaunchEvent::Running { game, .. } if *game == g.id));
    assert_eq!(launcher.running().len(), 1);
    let sessions = sink.all();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].source, SessionSource::Grapple);
    assert!(sessions[0].ended_at.is_none());

    launcher.stop(&g.id).await.unwrap();
    let ev = next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
    assert!(matches!(ev, LaunchEvent::Exited { game, .. } if game == g.id));
    assert!(launcher.running().is_empty());
    let sessions = sink.all();
    assert!(sessions[0].ended_at.is_some());
    // stopping again says it is not running
    assert!(matches!(launcher.stop(&g.id).await, Err(LaunchError::NotRunning(_))));
}

#[tokio::test]
async fn a_game_that_dies_at_once_is_a_failed_launch_with_the_log_path() {
    let dir = tempfile::tempdir().unwrap();
    let id = "t-dies";
    let mut settings = base_settings(dir.path());
    settings.manual.insert(
        GameId::new(Store::Manual, id),
        ManualEntry { exe: "/bin/sh".into(), args: vec!["-c".into(), "echo boom >&2; exit 3".into()], kind: ManualKind::Native, ..Default::default() },
    );
    let sink = Arc::new(MemorySink::new());
    let launcher = Launcher::with_sink(sink.clone(), settings);
    let g = game(Store::Manual, id);
    let (tx, mut rx) = mpsc::channel(64);
    let err = launcher.launch(&g, tx).await.unwrap_err();
    assert!(matches!(err, LaunchError::ExitedEarly { .. }), "{err:?}");
    let failed = next_matching(&mut rx, 1, |e| matches!(e, LaunchEvent::Failed { .. })).await;
    let LaunchEvent::Failed { message, .. } = failed else { unreachable!() };
    assert!(message.contains("exit code 3") && message.contains("manual-t-dies.log"), "{message}");
    assert!(sink.all().is_empty(), "no session for a failed launch");
    assert!(std::fs::read_to_string(dir.path().join("logs/manual-t-dies.log")).unwrap().contains("boom"));
}

#[tokio::test]
async fn the_game_is_followed_after_its_launcher_process_exits() {
    // The root exits after 2 s, the real game (a child) lives on: the environment marker keeps it tracked.
    let dir = tempfile::tempdir().unwrap();
    let id = "t-handoff";
    let mut settings = base_settings(dir.path());
    settings.manual.insert(
        GameId::new(Store::Manual, id),
        ManualEntry {
            exe: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 40 & sleep 2".into()],
            kind: ManualKind::Native,
            ..Default::default()
        },
    );
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), settings);
    let g = game(Store::Manual, id);
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(launcher.running().len(), 1, "still running after the root process exited");
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}

fn fake_steam(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    // `steam` stand-in. Records its arguments (one line per call) and, per call, its parent pid and environment.
    // Without `-applaunch` and with no client running it starts a fake client: a script named `steam` (so its
    // comm is "steam") that writes the pid file at once but opens the command pipe only after half a second, like
    // a real client that is still booting. `-applaunch <id>` before the pipe is open is recorded as EARLY; after,
    // it starts a harmless "game" process carrying SteamAppId.
    let bin = dir.join("bin");
    let args_file = dir.join("steam-args.txt");
    let pid_file = dir.join("steam.pid");
    let pipe = dir.join("steam.pipe");
    let client = dir.join("client");
    write_exec(
        &client.join("steam"),
        &format!(
            r#"#!/bin/sh
sleep 0.5
[ -p '{pipe}' ] || mkfifo '{pipe}'
exec 3<>'{pipe}'
trap 'kill $! 2>/dev/null; exit 0' TERM
sleep "$1" &
wait
"#,
            pipe = pipe.display()
        ),
    );
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> '{args}'
echo "$PPID" >> '{dir}/steam-ppid.txt'
env > '{dir}/steam-env.txt'
if [ "$1" = "-shutdown" ]; then
  [ -f '{pid}' ] && kill "$(cat '{pid}')" 2>/dev/null
  exit 0
fi
running() {{ [ -f '{pid}' ] && kill -0 "$(cat '{pid}')" 2>/dev/null; }}
ready() {{ running && ls -l /proc/"$(cat '{pid}')"/fd 2>/dev/null | grep -q '{pipe}'; }}
for a; do last=$a; done
case " $* " in
  *" -applaunch "*)
    if ready; then SteamAppId=$last setsid sleep 40 >/dev/null 2>&1 &
    else echo EARLY >> '{args}'; fi ;;
  *)
    if ! running; then
      setsid '{client}/steam' 60 >/dev/null 2>&1 &
      echo $! > '{pid}'
    fi ;;
esac
exit 0
"#,
        args = args_file.display(),
        dir = dir.display(),
        pid = pid_file.display(),
        pipe = pipe.display(),
        client = client.display()
    );
    write_exec(&bin.join("steam"), &script);
    (bin.join("steam"), args_file, pid_file)
}

fn steam_launcher(dir: &Path) -> (Launcher, Arc<MemorySink>, PathBuf, PathBuf) {
    let (bin, args_file, pid_file) = fake_steam(dir);
    let mut settings = base_settings(dir);
    settings.steam.bin = bin.to_string_lossy().into_owned();
    settings.steam.pid_file = pid_file.clone();
    settings.steam.pipe_file = dir.join("steam.pipe");
    let sink = Arc::new(MemorySink::new());
    let launcher = Launcher::with_sink(sink.clone(), settings.clone());
    launcher.set_strategy(Store::Steam, Arc::new(SteamStrategy::new(settings.steam.clone())));
    (launcher, sink, args_file, pid_file)
}

fn pid_alive(pid_file: &Path) -> bool {
    let Ok(s) = std::fs::read_to_string(pid_file) else { return false };
    let Ok(pid) = s.trim().parse::<i32>() else { return false };
    // zombies count as dead
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|st| !st.contains(") Z ")).unwrap_or(false)
}

#[tokio::test]
async fn steam_cold_start_is_silent_tracked_and_steam_is_shut_down_after_the_game() {
    let _turn = STEAM_TESTS.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (launcher, sink, args_file, pid_file) = steam_launcher(dir.path());
    let g = game(Store::Steam, "999001");
    let (tx, mut rx) = mpsc::channel(64);

    launcher.launch(&g, tx).await.unwrap();
    let events = drain(&mut rx);
    assert_eq!(events[0], step("Fetching game from Steam", false));
    assert_eq!(events[1], step("Fetching game from Steam", true));
    assert_eq!(events[2], step("Game files ready", true));
    assert_eq!(events[3], step("Starting game", false));
    assert_eq!(events[4], step("Starting game", true));
    assert!(matches!(events[5], LaunchEvent::Running { .. }));
    // Two steps: Steam alone and hidden, then (once its pipe is open, never EARLY) the game.
    assert_eq!(std::fs::read_to_string(&args_file).unwrap().trim(), "-silent -nofriendsui\n-applaunch 999001");
    assert_eq!(sink.all()[0].source, SessionSource::Grapple);
    // Steam is not Grapple's child, and it does not see what Grapple set for its own window.
    let me = std::process::id().to_string();
    let ppids = std::fs::read_to_string(dir.path().join("steam-ppid.txt")).unwrap();
    assert!(ppids.lines().all(|p| p.trim() != me), "steam must not be a child of grapple: {ppids}");
    let env = std::fs::read_to_string(dir.path().join("steam-env.txt")).unwrap();
    assert!(env.lines().any(|l| l.starts_with("HOME=")), "session environment is passed on");
    assert!(!env.lines().any(|l| l.starts_with("WEBKIT_") || l.starts_with("GRAPPLE_")), "{env}");

    // An Epic game run through umu carries SteamAppId too (umu derives it from GAMEID=umu-<id>); it is not a
    // Steam game and must not keep Steam open.
    let mut umu_game = std::process::Command::new("sleep")
        .arg("30")
        .envs([("SteamAppId", "999077"), ("SteamGameId", "999077"), ("UMU_ID", "umu-999077"), ("GRAPPLE_GAME", "epic:TestUmu")])
        .spawn()
        .unwrap();

    // stop signals the game only, never the Steam client
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
    assert!(pid_alive(&pid_file), "stop() must not touch the Steam client");

    // Grapple started this Steam, so the default policy shuts it down (cleanly: `steam -shutdown`)
    for _ in 0..50 {
        if std::fs::read_to_string(&args_file).unwrap().contains("-shutdown") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    umu_game.kill().ok();
    umu_game.wait().ok();
    let args = std::fs::read_to_string(&args_file).unwrap();
    assert!(args.lines().any(|l| l.trim() == "-shutdown"), "{args}");
}

#[tokio::test]
async fn steam_warm_launch_has_no_silent_flag_and_a_steam_started_by_the_user_stays_open() {
    let _turn = STEAM_TESTS.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (launcher, _sink, args_file, pid_file) = steam_launcher(dir.path());
    // Steam already running (started by "the user"): a client process and its pid file
    let client = dir.path().join("client").join("steam");
    let mut user_steam = std::process::Command::new(&client).arg("60").spawn().unwrap();
    // the kernel sets the process name a moment after exec returns control to the parent
    for _ in 0..200 {
        if std::fs::read_to_string(format!("/proc/{}/comm", user_steam.id())).map(|c| c.trim() == "steam").unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(&pid_file, user_steam.id().to_string()).unwrap();
    // ... and wait until it is ready (has its command pipe open)
    let pipe = dir.path().join("steam.pipe");
    let has_pipe = || {
        std::fs::read_dir(format!("/proc/{}/fd", user_steam.id()))
            .map(|rd| rd.filter_map(|e| e.ok()).any(|e| std::fs::read_link(e.path()).is_ok_and(|t| t == pipe)))
            .unwrap_or(false)
    };
    for _ in 0..300 {
        if has_pipe() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(has_pipe(), "fake user Steam never opened its pipe");

    let g = game(Store::Steam, "999003");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    let events = drain(&mut rx);
    assert_eq!(events[0], step("Fetching game from Steam", true), "already running: ready immediately");
    assert_eq!(std::fs::read_to_string(&args_file).unwrap().trim(), "-applaunch 999003");

    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!std::fs::read_to_string(&args_file).unwrap().contains("-shutdown"), "user's own Steam is left alone");
    assert!(pid_alive(&pid_file));
    user_steam.kill().ok();
    user_steam.wait().ok();
}

#[tokio::test]
async fn watcher_detects_games_started_elsewhere_and_ignores_unknown_ones() {
    let _turn = STEAM_TESTS.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(MemorySink::new());
    let launcher = Launcher::with_sink(sink.clone(), base_settings(dir.path()));
    launcher.set_known_games(&[game(Store::Steam, "999002")]);
    let (tx, mut rx) = mpsc::channel(64);
    launcher.spawn_watcher(tx);

    let mut unknown = std::process::Command::new("sleep").arg("30").env("SteamAppId", "999099").spawn().unwrap();
    let mut known = std::process::Command::new("sleep").arg("30").env("SteamAppId", "999002").spawn().unwrap();

    let ev = next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Running { .. })).await;
    assert!(matches!(&ev, LaunchEvent::Running { game, .. } if game.0 == "steam:999002"), "{ev:?}");
    let s = sink.all();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].source, SessionSource::Detected);
    assert_eq!(launcher.running().len(), 1, "the unknown app is not tracked");

    known.kill().unwrap();
    known.wait().unwrap();
    unknown.kill().unwrap();
    unknown.wait().unwrap();
    let ev = next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
    assert!(matches!(&ev, LaunchEvent::Exited { game, .. } if game.0 == "steam:999002"));
    assert!(sink.all()[0].ended_at.is_some());
}

#[tokio::test]
async fn startup_closes_crashed_sessions_at_last_seen_and_readopts_running_games() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(MemorySink::new());
    let dead = sink.insert_open(&GameId::new(Store::Steam, "999005"), 1_000, SessionSource::Grapple, Some(1_500));
    let dead_no_hb = sink.insert_open(&GameId::new(Store::Epic, "Ghost"), 2_000, SessionSource::Detected, None);
    let live_id = GameId::new(Store::Manual, "t-recover");
    let live = sink.insert_open(&live_id, 3_000, SessionSource::Grapple, Some(3_100));
    let mut proc = std::process::Command::new("sleep").arg("30").env("GRAPPLE_GAME", &live_id.0).spawn().unwrap();

    let launcher = Launcher::with_sink(sink.clone(), base_settings(dir.path()));
    let (tx, mut rx) = mpsc::channel(8);
    launcher.recover(&tx).await.unwrap();

    let all = sink.all();
    let get = |id| all.iter().find(|s| s.id == id).unwrap().clone();
    assert_eq!(get(dead).ended_at, Some(1_500), "closed at the last time it was seen");
    assert_eq!(get(dead_no_hb).ended_at, Some(2_000), "no heartbeat: zero length, never 'until now'");
    assert_eq!(get(live).ended_at, None, "still running: left open");
    assert_eq!(launcher.running(), vec![(live_id.clone(), 3_000)]);
    assert!(matches!(rx.try_recv(), Ok(LaunchEvent::Running { game, started_at: 3_000 }) if game == live_id));
    proc.kill().unwrap();
    proc.wait().unwrap();
}

// ------------------------------------------------------------------ Epic end to end

struct EpicFixture {
    dir: tempfile::TempDir,
    settings: LaunchSettings,
    args_file: PathBuf,
    env_file: PathBuf,
}

fn epic_fixture(app: &str, api: EpicApi, can_run_offline: bool, expires_at: &str) -> EpicFixture {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let heroic = p.join("heroic");
    let legendary = p.join("legendary");
    let gamedir = p.join("game");
    std::fs::create_dir_all(gamedir.join("Bin/Win64")).unwrap();
    write_exec(&gamedir.join("Bin/Win64/Game.exe"), "x");
    write_exec(&p.join("proton/proton"), "#!/bin/sh\n");
    write(
        &heroic.join("config.json"),
        r#"{"defaultSettings":{"language":"en","enableEsync":true,"enableFsync":true}}"#,
    );
    write(
        &heroic.join(format!("GamesConfig/{app}.json")),
        &format!(
            r#"{{"{app}":{{"wineVersion":{{"bin":"{proton}","name":"GE","type":"proton"}},"winePrefix":"{pfx}","useGameMode":false,
"enviromentOptions":[{{"key":"FOO","value":"bar"}}],"launcherArgs":"-windowed","lastUsedLaunchOption":{{"type":"basic","parameters":""}}}}}}"#,
            app = app,
            proton = p.join("proton/proton").display(),
            pfx = p.join("pfx").display()
        ),
    );
    write(
        &legendary.join("installed.json"),
        &format!(
            r#"{{"{app}":{{"app_name":"{app}","title":"Test Game","install_path":"{g}","executable":"Bin\\Win64\\Game.exe",
"launch_parameters":"-fromjson","can_run_offline":{can_run_offline},"requires_ot":false,"platform":"Windows"}}}}"#,
            app = app,
            g = gamedir.display()
        ),
    );
    write(
        &legendary.join(format!("metadata/{app}.json")),
        r#"{"app_name":"APP","metadata":{"namespace":"nsX","id":"catY","customAttributes":{"AdditionalCommandLine":{"value":"-extra1 \"two words\""}}},"sidecar":{"config":{"deploymentId":"dep9"}}}"#.replace("APP", app).as_str(),
    );
    write(
        &legendary.join("user.json"),
        &format!(
            r#"{{"access_token":"OLD-ACCESS","refresh_token":"OLD-REFRESH","account_id":"acc123","displayName":"Tester","expires_at":"{expires_at}","other":"keep"}}"#
        ),
    );
    let args_file = p.join("umu-args.txt");
    let env_file = p.join("umu-env.txt");
    write_exec(
        &p.join("umu-run"),
        &format!(
            "#!/bin/sh\nfor a; do echo \"$a\"; done > '{}'\nenv > '{}'\npwd >> '{}'\necho \"INFO: Running 'GE' using runtime 'steamrt4'\"\nexec sleep 40\n",
            args_file.display(),
            env_file.display(),
            args_file.display()
        ),
    );
    let mut settings = base_settings(p);
    settings.umu_run = Some(p.join("umu-run"));
    settings.epic_api = api;
    EpicFixture { dir, settings, args_file, env_file }
}

fn api(base: &str) -> EpicApi {
    EpicApi { oauth_base: base.into(), ecommerce_base: base.into(), ..EpicApi::default() }
}

#[tokio::test]
async fn epic_launch_signs_in_runs_through_umu_with_legendarys_arguments() {
    let server = MockServer::start(|r| match r.path.as_str() {
        "/account/api/oauth/token" => (
            200,
            r#"{"access_token":"NEW-ACCESS","refresh_token":"NEW-REFRESH","expires_at":"2099-01-01T00:00:00.000Z","account_id":"acc123","displayName":"Tester"}"#.into(),
        ),
        "/account/api/oauth/exchange" => (200, r#"{"code":"EXCHANGE-E2E"}"#.into()),
        _ => (404, "{}".into()),
    })
    .await;
    let fx = epic_fixture("TestOnline", api(&server.base), true, "2000-01-01T00:00:00.000Z");
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let g = game(Store::Epic, "TestOnline");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();

    let events = drain(&mut rx);
    let labels: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            LaunchEvent::Step { label, done } => Some((label.as_str(), *done)),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("Epic sign-in token", false),
            ("Epic sign-in token", true),
            ("Proton ready", false),
            ("Proton ready", true),
            ("Starting game", false),
            ("Starting game", true)
        ]
    );
    assert!(matches!(events.last(), Some(LaunchEvent::Running { .. })));
    assert!(!format!("{events:?}").contains("EXCHANGE-E2E"), "the exchange code never reaches events");

    // legendary's order: exe, installed.json params, metadata params, user params, egl params
    let text = std::fs::read_to_string(&fx.args_file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let exe = fx.dir.path().join("game/Bin/Win64/Game.exe");
    assert_eq!(
        &lines[..13],
        [
            exe.to_str().unwrap(),
            "-fromjson",
            "-extra1",
            "\"two words\"",
            "-windowed",
            "-AUTH_LOGIN=unused",
            "-AUTH_PASSWORD=EXCHANGE-E2E",
            "-AUTH_TYPE=exchangecode",
            "-epicapp=TestOnline",
            "-epicenv=Prod",
            "-EpicPortal",
            "-epicusername=Tester",
            "-epicuserid=acc123",
        ]
    );
    assert_eq!(&lines[13..], ["-epiclocale=en", "-epicsandboxid=nsX", "-epicdeploymentid=dep9", fx.dir.path().join("game/Bin/Win64").to_str().unwrap()]);

    let env = std::fs::read_to_string(&fx.env_file).unwrap();
    let pfx = fx.dir.path().join("pfx");
    for want in [
        format!("STEAM_COMPAT_DATA_PATH={}", pfx.display()),
        format!("WINEPREFIX={}", pfx.display()),
        format!("PROTONPATH={}", fx.dir.path().join("proton").display()),
        "GAMEID=umu-0".to_string(),
        "STORE=egs".to_string(),
        "FOO=bar".to_string(),
        "GRAPPLE_GAME=epic:TestOnline".to_string(),
        "SteamAppId=0".to_string(),
    ] {
        assert!(env.lines().any(|l| l == want), "missing {want}");
    }

    // rotated tokens were written back, other fields kept
    let uj: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.dir.path().join("legendary/user.json")).unwrap()).unwrap();
    assert_eq!(uj["refresh_token"], "NEW-REFRESH");
    assert_eq!(uj["other"], "keep");

    launcher.stop(&g.id).await.unwrap();
    // the Epic game never looks like a Steam game to the tracker
    assert!(launcher.running().len() <= 1);
}

#[tokio::test]
async fn epic_offline_fallback_starts_without_a_token_when_the_game_allows_it() {
    let fx = epic_fixture("TestOffline", api("http://127.0.0.1:1"), true, "2000-01-01T00:00:00.000Z");
    let before = std::fs::read(fx.dir.path().join("legendary/user.json")).unwrap();
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let g = game(Store::Epic, "TestOffline");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    let text = std::fs::read_to_string(&fx.args_file).unwrap();
    assert!(text.lines().any(|l| l == "-AUTH_PASSWORD="), "{text}");
    assert_eq!(std::fs::read(fx.dir.path().join("legendary/user.json")).unwrap(), before);
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}

#[tokio::test]
async fn epic_without_offline_support_fails_clearly_when_epic_is_unreachable() {
    let fx = epic_fixture("TestNoOffline", api("http://127.0.0.1:1"), false, "2000-01-01T00:00:00.000Z");
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let (tx, mut rx) = mpsc::channel(64);
    let err = launcher.launch(&game(Store::Epic, "TestNoOffline"), tx).await.unwrap_err();
    assert!(matches!(err, LaunchError::Network(_)), "{err:?}");
    next_matching(&mut rx, 1, |e| matches!(e, LaunchEvent::Failed { .. })).await;
    assert!(!fx.args_file.exists(), "nothing was started");
}

// ---------------------------------------------------------------- umu's Steam Linux Runtime

/// A fake umu-run that prints `lines` (umu 1.4.0's own messages) with pauses, then becomes the "game".
fn umu_script(fx: &EpicFixture, body: &str) {
    write_exec(&fx.dir.path().join("umu-run"), &format!("#!/bin/sh\nenv > '{}'\n{body}", fx.env_file.display()));
}

fn steps_of(events: &[LaunchEvent]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            LaunchEvent::Step { label, done } => Some((label.clone(), *done)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_runtime_download_is_a_launch_step_and_not_the_game_starting() {
    let fx = epic_fixture("TestRuntime", api("http://127.0.0.1:1"), true, "2000-01-01T00:00:00.000Z");
    // Longer than min_alive (2.5 s): before, umu's python counted as the game and the launch said "running".
    umu_script(
        &fx,
        "echo 'INFO: umu-launcher version 1.4.0'\necho 'INFO: Updating steamrt4 to latest...'\n\
         echo 'INFO: Downloading steamrt4 (latest), please wait...'\nsleep 4\n\
         echo 'INFO: SteamLinuxRuntime_4.tar.xz: SHA256 is OK'\necho 'INFO: steamrt4 is up to date'\n\
         echo \"INFO: Running 'GE' using runtime 'steamrt4'\"\nexec sleep 40\n",
    );
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let g = game(Store::Epic, "TestRuntime");
    let (tx, mut rx) = mpsc::channel(64);
    let t0 = std::time::Instant::now();
    launcher.launch(&g, tx).await.unwrap();
    assert!(t0.elapsed() >= Duration::from_secs(4), "running only after the download");
    let events = drain(&mut rx);
    let steps = steps_of(&events);
    let pos = |label: &str, done: bool| steps.iter().position(|s| s.0 == label && s.1 == done).unwrap_or_else(|| panic!("{label} {done}: {steps:?}"));
    assert!(pos("Starting game", false) < pos("Updating Steam Linux Runtime", false));
    assert!(pos("Updating Steam Linux Runtime", false) < pos("Updating Steam Linux Runtime", true));
    assert!(pos("Updating Steam Linux Runtime", true) < pos("Starting game", true));
    assert!(matches!(events.last(), Some(LaunchEvent::Running { .. })));
    // no runtime installed (empty umu dir): umu is left to download it
    let env = std::fs::read_to_string(&fx.env_file).unwrap();
    assert!(!env.lines().any(|l| l.starts_with("UMU_RUNTIME_UPDATE=")), "{env}");
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}

#[tokio::test]
async fn a_failed_runtime_download_is_a_clear_failure() {
    let fx = epic_fixture("TestRuntimeFail", api("http://127.0.0.1:1"), true, "2000-01-01T00:00:00.000Z");
    umu_script(
        &fx,
        "echo 'INFO: Setting up Unified Launcher for Windows Games on Linux...'\nsleep 3\n\
         echo 'INFO: Connection broken, trying to resume /tmp/x/SteamLinuxRuntime_4.tar.xz.4.0.1.parts (retry 1)'\n\
         echo 'ERROR: Aborting steamrt install due to network error'\n\
         echo 'RuntimeError: umu has not been setup for the user' >&2\nexit 1\n",
    );
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let (tx, mut rx) = mpsc::channel(64);
    let err = launcher.launch(&game(Store::Epic, "TestRuntimeFail"), tx).await.unwrap_err();
    assert!(matches!(err, LaunchError::RuntimeDownload), "{err:?}");
    let failed = next_matching(&mut rx, 1, |e| matches!(e, LaunchEvent::Failed { .. })).await;
    let LaunchEvent::Failed { message, .. } = failed else { unreachable!() };
    assert_eq!(message, "Runtime download failed. Check your connection.");
    assert!(launcher.running().is_empty());
}

#[tokio::test]
async fn an_installed_runtime_starts_without_umus_update_check() {
    let fx = epic_fixture("TestRuntimeReady", api("http://127.0.0.1:1"), true, "2000-01-01T00:00:00.000Z");
    let p = fx.dir.path();
    // the fixture's Proton needs steamrt4, which is installed completely
    write(&p.join("proton/toolmanifest.vdf"), "\"manifest\"\n{\n  \"commandline\" \"/proton %verb%\"\n  \"require_tool_appid\" \"4183110\"\n}\n");
    let rt = fx.settings.umu_dir.join("steamrt4");
    for f in [".installed.ok", "mtree.txt.gz", "VERSIONS.txt", "toolmanifest.vdf", "_v2-entry-point"] {
        write(&rt.join(f), "x");
    }
    std::fs::create_dir_all(rt.join("pressure-vessel")).unwrap();
    std::fs::create_dir_all(rt.join("steamrt4_platform_4.0.20261110.268882/files")).unwrap();
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), fx.settings.clone());
    let g = game(Store::Epic, "TestRuntimeReady");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    let env = std::fs::read_to_string(&fx.env_file).unwrap();
    assert!(env.lines().any(|l| l == "UMU_RUNTIME_UPDATE=0"), "{env}");
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}

const RT4: grapple_launch::umu::Runtime = grapple_launch::umu::Runtime { variant: "steamrt4", codename: "steamrt4" };

/// A fake umu-run for the background updater: records its arguments and environment, prints `body`.
fn updater_fixture(body: &str) -> (tempfile::TempDir, LaunchSettings) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let mut settings = base_settings(p);
    std::fs::create_dir_all(settings.umu_dir.join("steamrt4")).unwrap();
    write_exec(
        &p.join("umu-run"),
        &format!("#!/bin/sh\necho \"$@\" > '{}'\nenv > '{}'\n{body}", p.join("args.txt").display(), p.join("env.txt").display()),
    );
    settings.umu_run = Some(p.join("umu-run"));
    (dir, settings)
}

#[tokio::test]
async fn the_background_update_runs_umus_updater_and_records_the_check() {
    let (dir, settings) = updater_fixture("echo 'INFO: Updating steamrt4 to latest...'\necho 'INFO: steamrt4 is up to date'\necho \"INFO: Running 'umu-steamrt4' using runtime 'steamrt4'\"\n");
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), settings.clone());
    assert!(launcher.update_runtime_now(RT4).await.unwrap());
    assert_eq!(std::fs::read_to_string(dir.path().join("args.txt")).unwrap().trim(), "true");
    let env = std::fs::read_to_string(dir.path().join("env.txt")).unwrap();
    for want in ["UMU_NO_PROTON=1", "RUNTIMEPATH=steamrt4", "UMU_ZENITY="] {
        assert!(env.lines().any(|l| l == want), "missing {want}");
    }
    assert!(settings.log_dir.join("umu-steamrt4.log").is_file(), "logged to grapple's log dir");
    assert!(settings.log_dir.join("umu-steamrt4.checked").is_file());
    // checked today: the next round does nothing
    std::fs::remove_file(dir.path().join("args.txt")).unwrap();
    launcher.update_runtimes_if_due().await;
    assert!(!dir.path().join("args.txt").exists());
}

#[tokio::test]
async fn the_background_update_waits_while_a_game_runs() {
    let (dir, mut settings) = updater_fixture("echo 'INFO: steamrt4 is up to date'\n");
    let id = GameId::new(Store::Manual, "t-busy");
    settings.manual.insert(id.clone(), ManualEntry { exe: sleep_bin(), args: vec!["30".into()], kind: ManualKind::Native, ..Default::default() });
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), settings);
    let g = game(Store::Manual, "t-busy");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    launcher.update_runtimes_if_due().await;
    assert!(!dir.path().join("args.txt").exists(), "nothing runs umu while a game is running");
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}

#[tokio::test]
async fn a_launch_stops_a_background_download() {
    let (dir, mut settings) = updater_fixture("echo 'INFO: Updating steamrt4 to latest...'\necho 'INFO: Downloading steamrt4 (latest), please wait...'\nexec sleep 60\n");
    let id = GameId::new(Store::Manual, "t-yield");
    settings.manual.insert(id.clone(), ManualEntry { exe: sleep_bin(), args: vec!["30".into()], kind: ManualKind::Native, ..Default::default() });
    let log = settings.log_dir.join("umu-steamrt4.log");
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), settings);
    let bg = launcher.clone();
    let update = tokio::spawn(async move { bg.update_runtime_now(RT4).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("please wait") {
        assert!(std::time::Instant::now() < deadline, "the updater never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let g = game(Store::Manual, "t-yield");
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(5), update).await.expect("the update was stopped").unwrap();
    assert!(!done.unwrap(), "not current: it starts over another time");
    assert!(!dir.path().join("logs/umu-steamrt4.checked").exists());
    launcher.stop(&g.id).await.unwrap();
    next_matching(&mut rx, 10, |e| matches!(e, LaunchEvent::Exited { .. })).await;
}
