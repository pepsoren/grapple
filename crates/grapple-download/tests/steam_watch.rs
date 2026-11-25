//! The Steam watcher on a generated Steam folder: Josh's real content_log lines and appmanifest, two libraries.

use grapple_download::steam_watch::{SteamPhase, SteamKind, SteamWatch, classify, parse_acf};
use std::path::Path;

const FINISHED_ACF: &str = "\"AppState\"\n{\n\t\"appid\"\t\t\"5246870\"\n\t\"name\"\t\t\"We Are So Cooked Demo\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"We Are So Cooked Demo\"\n\t\"SizeOnDisk\"\t\t\"1104001082\"\n\t\"buildid\"\t\t\"26041765\"\n\t\"BytesToDownload\"\t\t\"394704576\"\n\t\"BytesDownloaded\"\t\t\"394704576\"\n\t\"BytesToStage\"\t\t\"1884395143\"\n\t\"BytesStaged\"\t\t\"1884395143\"\n\t\"TargetBuildID\"\t\t\"26041765\"\n\t\"AutoUpdateBehavior\"\t\t\"0\"\n}\n";

const LOG: &str = include_str!("fixtures/content_log.txt");

fn acf(appid: u32, name: &str, flags: u64, to_dl: u64, dl: u64) -> String {
    format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"{name}\"\n\t\"buildid\"\t\t\"1\"\n\t\"BytesToDownload\"\t\t\"{to_dl}\"\n\t\"BytesDownloaded\"\t\t\"{dl}\"\n\t\"BytesToStage\"\t\t\"{}\"\n\t\"BytesStaged\"\t\t\"0\"\n}}\n",
        to_dl * 2
    )
}

fn steam_root(dir: &Path) -> std::path::PathBuf {
    let root = dir.join("Steam");
    let lib2 = dir.join("SteamLibrary");
    for d in [root.join("steamapps"), lib2.join("steamapps"), root.join("logs")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        root.join("steamapps/libraryfolders.vdf"),
        format!("\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n", root.display(), lib2.display()),
    )
    .unwrap();
    root
}

#[test]
fn the_real_finished_manifest_is_not_a_download() {
    let a = parse_acf(FINISHED_ACF, Path::new("/data/SteamLibrary")).unwrap();
    assert_eq!((a.appid, a.state_flags, a.bytes_to_download, a.buildid), (5246870, 4, 394704576, 26041765));
    assert!(classify(&a, 0).is_none());
    // Installed + update required (6): an update waiting.
    let mut u = a.clone();
    u.state_flags = 6;
    let c = classify(&u, 0).unwrap();
    assert_eq!((c.kind, c.phase, c.update_available), (SteamKind::Update, SteamPhase::Queued, true));
    // 1026 / 1030: running.
    for f in [1026, 1030] {
        u.state_flags = f;
        let c = classify(&u, 0).unwrap();
        assert_eq!(c.phase, SteamPhase::Downloading);
        assert_eq!(c.kind, if f == 1030 { SteamKind::Update } else { SteamKind::Install });
    }
    // The staging folder's size, scaled to download bytes, moves progress between acf writes.
    u.bytes_downloaded = 0;
    let c = classify(&u, 1884395143 / 2).unwrap();
    assert!((c.done as i64 - 394704576 / 2).abs() <= 1, "{}", c.done);
}

#[test]
fn watcher_follows_a_download_to_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = steam_root(tmp.path());
    let lib2 = tmp.path().join("SteamLibrary");
    std::fs::write(lib2.join("steamapps/appmanifest_5246870.acf"), acf(5246870, "We Are So Cooked Demo", 1026, 0, 0)).unwrap();
    std::fs::write(root.join("steamapps/appmanifest_1478500.acf"), acf(1478500, "Big Walk", 6, 1_340_000_000, 0)).unwrap();
    std::fs::write(root.join("steamapps/appmanifest_730.acf"), acf(730, "Counter-Strike 2", 4, 0, 0)).unwrap();
    std::fs::write(root.join("steamapps/appmanifest_892970.acf"), acf(892970, "Valheim", 2 | 512, 1_600_000_000, 100)).unwrap();
    // The first lines of the real log: the download has started.
    let first: String = LOG.lines().take(12).map(|l| format!("{l}\n")).collect();
    std::fs::write(root.join("logs/content_log.txt"), &first).unwrap();

    let mut w = SteamWatch::new(root.clone(), true);
    assert_eq!(w.libraries().len(), 2);
    let r = w.poll();
    let by = |id: u32| r.apps.iter().find(|a| a.appid == id).cloned();
    let demo = by(5246870).unwrap();
    assert_eq!((demo.kind, demo.phase), (SteamKind::Install, SteamPhase::Downloading));
    assert_eq!(demo.total, Some(394633216), "total from the log while the acf has none");
    assert_eq!(demo.library, lib2);
    let big = by(1478500).unwrap();
    assert!(big.update_available && big.total == Some(1_340_000_000));
    assert_eq!(by(892970).unwrap().phase, SteamPhase::Paused);
    assert!(by(730).is_none());
    assert!(r.finished.is_empty());

    // Steam writes progress; the next poll has a speed.
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(lib2.join("steamapps/appmanifest_5246870.acf"), acf(5246870, "We Are So Cooked Demo", 1026, 394633216, 100_000_000)).unwrap();
    let r = w.poll();
    let demo = r.apps.iter().find(|a| a.appid == 5246870).unwrap();
    assert_eq!(demo.done, 100_000_000);
    assert!(demo.bytes_per_sec.unwrap() > 0 && demo.seconds_left.is_some());

    // The rest of the log, and the finished manifest.
    std::fs::write(root.join("logs/content_log.txt"), LOG).unwrap();
    std::fs::write(lib2.join("steamapps/appmanifest_5246870.acf"), FINISHED_ACF).unwrap();
    let r = w.poll();
    assert_eq!(r.finished, vec![5246870]);
    assert!(r.apps.iter().all(|a| a.appid != 5246870));

    // Steam rotated the log: reading starts over without trouble.
    std::fs::write(root.join("logs/content_log.txt"), "[2026-10-20 10:00:00] AppID 1 state changed : Update Required,\n").unwrap();
    let r = w.poll();
    assert!(r.finished.is_empty());
}
