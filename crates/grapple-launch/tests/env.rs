//! What the processes Grapple starts see: the session environment Grapple started with, without anything
//! Grapple set for its own window (`WEBKIT_*`, `__NV_*`) or its own switches (`GRAPPLE_*`), plus the
//! `GRAPPLE_GAME` marker. Its own test binary: it changes the process environment.

// Linux (and other Unix) only: the environment Steam and games get is checked with shell scripts and /proc.
#![cfg(unix)]

mod common;

use common::*;
use grapple_core::{Art, Game, GameId, LaunchEvent, Store};
use grapple_launch::manual::{ManualEntry, ManualKind};
use grapple_launch::steam::SteamSettings;
use grapple_launch::{LaunchSettings, Launcher, MemorySink};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn env_of(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn assert_clean(env: &[(String, String)], what: &str) {
    let has = |k: &str| env.iter().any(|(x, _)| x == k);
    assert!(env.iter().any(|(k, v)| k == "SESSION_MARKER" && v == "kept"), "{what}: the session environment is passed on");
    for (k, _) in env {
        assert!(!k.starts_with("WEBKIT_"), "{what} sees {k}");
        assert!(k != "__NV_DISABLE_EXPLICIT_SYNC", "{what} sees {k}");
        assert!(k == "GRAPPLE_GAME" || !k.starts_with("GRAPPLE_"), "{what} sees {k}");
    }
    assert!(!has("LATE_SHELL_INTERNAL"), "{what} sees a variable set after start-up");
}

#[tokio::test]
async fn children_get_the_session_environment_without_grapples_additions() {
    // SAFETY: the only test in this binary; nothing else runs yet.
    unsafe {
        std::env::set_var("SESSION_MARKER", "kept");
        std::env::set_var("GRAPPLE_PERF", "1"); // a Grapple switch Josh set when starting it
    }
    grapple_launch::env::capture();
    // SAFETY: as above.
    unsafe {
        grapple_launch::env::set_own("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        grapple_launch::env::set_own("__NV_DISABLE_EXPLICIT_SYNC", "1");
        std::env::set_var("LATE_SHELL_INTERNAL", "1"); // what GTK/WebKit/Tauri set later
    }

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("game-env.txt");
    let id = GameId::new(Store::Manual, "env-check");
    let mut settings = LaunchSettings {
        heroic_dir: dir.path().join("heroic"),
        legendary_dir: dir.path().join("legendary"),
        log_dir: dir.path().join("logs"),
        umu_dir: dir.path().join("umu"),
        poll_interval: Duration::from_millis(100),
        steam: SteamSettings { bin: "/nonexistent/steam".into(), pid_file: dir.path().join("steam.pid"), ..SteamSettings::default() },
        ..LaunchSettings::default()
    };
    settings.manual.insert(
        id.clone(),
        ManualEntry {
            exe: "/bin/sh".into(),
            args: vec!["-c".into(), format!("env > '{}'; exec sleep 20", out.display())],
            kind: ManualKind::Native,
            ..Default::default()
        },
    );
    let launcher = Launcher::with_sink(Arc::new(MemorySink::new()), settings.clone());
    let g = Game {
        id: id.clone(),
        store: Store::Manual,
        name: "env".into(),
        sort_name: "env".into(),
        installed: true,
        install_dir: None,
        size_bytes: None,
        art: Art::default(),
        store_playtime_min: 0,
        last_played: None,
        status: None,
    };
    let (tx, mut rx) = mpsc::channel(64);
    launcher.launch(&g, tx).await.unwrap();
    let env = env_of(&std::fs::read_to_string(&out).unwrap());
    assert_clean(&env, "a game");
    assert!(env.iter().any(|(k, v)| k == "GRAPPLE_GAME" && v == &id.0), "the tracker's marker is set");

    // In its own systemd scope when the user manager can make one (else just its own session).
    if grapple_launch::spawn::scopes_available() {
        let marker = format!("GRAPPLE_GAME={}", id.0).into_bytes();
        let game_pid = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .find(|p| std::fs::read(format!("/proc/{p}/environ")).is_ok_and(|e| e.split(|b| *b == 0).any(|kv| kv == marker.as_slice())))
            .expect("the game process");
        let cg = std::fs::read_to_string(format!("/proc/{game_pid}/cgroup")).unwrap();
        assert!(cg.contains("app-grapple-sh-"), "own scope: {cg}");
    }
    launcher.stop(&id).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = rx.recv().await {
            if matches!(e, LaunchEvent::Exited { .. }) {
                break;
            }
        }
    })
    .await;

    // Steam commands (here `steam://install/…`) too, and Steam is not Grapple's child.
    let steam = dir.path().join("bin").join("steam");
    write_exec(
        &steam,
        &format!("#!/bin/sh\nenv > '{d}/steam-env.txt'\necho $PPID > '{d}/steam-ppid.txt'\n", d = dir.path().display()),
    );
    let s = SteamSettings { bin: steam.to_string_lossy().into_owned(), ..settings.steam.clone() };
    s.open_url("steam://install/999").unwrap();
    let file = dir.path().join("steam-ppid.txt");
    for _ in 0..100 {
        if file.exists() && !std::fs::read_to_string(&file).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_ne!(std::fs::read_to_string(&file).unwrap().trim(), std::process::id().to_string(), "steam is not grapple's child");
    assert_clean(&env_of(&std::fs::read_to_string(dir.path().join("steam-env.txt")).unwrap()), "steam");
}
