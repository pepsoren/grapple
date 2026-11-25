mod common;
use common::*;
use grapple_core::{GameStatus, Store};
use grapple_scan::{ScanConfig, ScanResult, scan_all};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const ICON_A: &str = "0123456789abcdef0123456789abcdef01234567"; // 40 hex
const ICON_B: &str = "fedcba9876543210fedcba9876543210fedcba98";
const HASH1: &str = "1ebfc7f05967d7dd39a7659b31b04a6b40a77663";
const HASH2: &str = "2b44644b033a996a9efb95051bb01809215d0d23";

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn build_steam(tmp: &Path) -> PathBuf {
    let root = tmp.join("steam");
    copy_fixture(&fixtures().join("steam"), &root, tmp);
    let cache = root.join("appcache/librarycache");
    // 100: art at the root, an extra portrait in a subfolder, two icons (clienticon picks ICON_B).
    for f in ["library_600x900.jpg", "library_hero.jpg", "logo.png", &format!("{ICON_A}.jpg"), &format!("{ICON_B}.jpg"), "header.jpg"] {
        touch(&cache.join("100").join(f));
    }
    touch(&cache.join("100").join(HASH1).join("library_capsule.jpg"));
    // 200: everything in hashed subfolders, capsule only (no 600x900).
    touch(&cache.join("200").join(HASH1).join("library_capsule.jpg"));
    touch(&cache.join("200").join(HASH2).join("library_hero.jpg"));
    touch(&cache.join("200").join(HASH2).join("logo.png"));
    // 400: owned, not installed, no art at all. 450: a demo. 500/600/800: dlc, tool, no appinfo entry. 700: application.
    for id in [400, 450, 500, 600, 700, 800] {
        std::fs::create_dir_all(cache.join(id.to_string())).unwrap();
    }
    touch(&cache.join("assetcache.vdf")); // a file, not an app folder
    touch(&cache.join("notanumber/library_hero.jpg"));
    let apps = vec![
        app_entry(100, "Alpha Game", "Game", Some(ICON_B)),
        app_entry(200, "The Beta Game", "game", None),
        app_entry(300, "Gamma", "game", None),
        app_entry(400, "Delta: Unowned?", "game", None),
        app_entry(450, "Epsilon Demo", "Demo", None),
        app_entry(500, "Alpha Soundtrack", "dlc", None),
        app_entry(600, "Some Tool", "tool", None),
        app_entry(700, "Wallpaper Thing", "application", None),
        app_entry(900, "Proton 9.0", "tool", None),
        app_entry(9999, "Not Known Locally", "game", None),
    ];
    std::fs::write(root.join("appcache/appinfo.vdf"), build_appinfo(29, &apps)).unwrap();
    root
}

fn steam_only(root: PathBuf) -> ScanConfig {
    ScanConfig { steam_root: Some(root), heroic_config: None, legendary_dir: None, egl_dir: None }
}

fn game<'a>(r: &'a ScanResult, id: &str) -> &'a grapple_core::Game {
    r.games.iter().find(|g| g.id.0 == id).unwrap_or_else(|| panic!("{id} missing from {:?}", r.games.iter().map(|g| &g.id.0).collect::<Vec<_>>()))
}

#[test]
fn steam_scan_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = build_steam(tmp.path());
    let r = scan_all(&steam_only(root.clone()));

    let ids: Vec<_> = r.games.iter().map(|g| g.id.0.as_str()).collect();
    // Sorted by sort_name: alpha game, the beta game -> "beta game", delta unowned, gamma.
    assert_eq!(ids, ["steam:100", "steam:200", "steam:400", "steam:450", "steam:300"], "demos count as games");
    assert_eq!(r.warnings, Vec::<String>::new());

    let a = game(&r, "steam:100");
    assert_eq!(a.store, Store::Steam);
    assert_eq!(a.name, "Alpha Game", "appinfo name beats the manifest name");
    assert_eq!(a.sort_name, "alpha game");
    assert!(a.installed);
    assert_eq!(a.install_dir.as_deref(), Some(root.join("steamapps/common/Alpha Game").as_path()));
    assert_eq!(a.size_bytes, Some(123456));
    assert_eq!(a.store_playtime_min, 125);
    assert_eq!(a.last_played, Some(1_700_001_000), "newer of localconfig and manifest");
    assert_eq!(a.status, None);
    let cache = root.join("appcache/librarycache/100");
    assert_eq!(a.art.cover.as_deref(), Some(cache.join("library_600x900.jpg").as_path()));
    assert_eq!(a.art.hero.as_deref(), Some(cache.join("library_hero.jpg").as_path()));
    assert_eq!(a.art.logo.as_deref(), Some(cache.join("logo.png").as_path()));
    assert_eq!(a.art.icon.as_deref(), Some(cache.join(format!("{ICON_B}.jpg")).as_path()), "clienticon picks the icon");

    let b = game(&r, "steam:200");
    assert_eq!(b.name, "The Beta Game");
    assert_eq!(b.sort_name, "beta game");
    assert!(b.installed);
    assert_eq!(b.install_dir.as_deref(), Some(root.join("lib2/steamapps/common/BetaGame").as_path()), "second library");
    assert_eq!(b.status, Some(GameStatus::UpdateQueued));
    assert_eq!(b.store_playtime_min, 7);
    assert_eq!(b.last_played, None);
    let cache = root.join("appcache/librarycache/200");
    assert_eq!(b.art.cover.as_deref(), Some(cache.join(HASH1).join("library_capsule.jpg").as_path()));
    assert_eq!(b.art.hero.as_deref(), Some(cache.join(HASH2).join("library_hero.jpg").as_path()));
    assert_eq!(b.art.logo.as_deref(), Some(cache.join(HASH2).join("logo.png").as_path()));
    assert_eq!(b.art.icon, None);

    let g = game(&r, "steam:300");
    assert!(g.installed, "flags 1030 still include FullyInstalled");
    assert_eq!(g.status, Some(GameStatus::Updating));
    assert_eq!(g.art, Default::default(), "no librarycache folder");

    let d = game(&r, "steam:400");
    assert!(!d.installed);
    assert_eq!(d.install_dir, None);
    assert_eq!(d.size_bytes, None);
    assert_eq!(d.store_playtime_min, 61);
    assert_eq!(d.last_played, Some(1_600_000_000));
    assert_eq!(d.name, "Delta: Unowned?");
    assert_eq!(d.sort_name, "delta unowned");
}

#[test]
fn steam_without_appinfo_falls_back_to_installed_and_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let root = build_steam(tmp.path());
    std::fs::remove_file(root.join("appcache/appinfo.vdf")).unwrap();
    let r = scan_all(&steam_only(root.clone()));
    let ids: Vec<_> = r.games.iter().map(|g| g.id.0.as_str()).collect();
    // Proton is installed but looks like a tool, so it is dropped; 100/200/300 come from their manifests.
    assert_eq!(ids, ["steam:200", "steam:300", "steam:100"]);
    assert_eq!(game(&r, "steam:100").name, "Old Manifest Name");
    assert!(r.warnings.iter().any(|w| w.contains("appinfo.vdf")), "{:?}", r.warnings);

    std::fs::write(root.join("appcache/appinfo.vdf"), b"this is not an appinfo file at all").unwrap();
    let r = scan_all(&steam_only(root));
    assert_eq!(r.games.len(), 3);
    assert!(r.warnings.iter().any(|w| w.contains("not an appinfo")), "{:?}", r.warnings);
}

#[test]
fn steam_missing_pieces_never_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let r = scan_all(&steam_only(tmp.path().join("nope")));
    assert!(r.games.is_empty());
    assert!(!r.warnings.is_empty());
    let r = scan_all(&ScanConfig::default());
    assert!(r.games.is_empty());
    assert_eq!(r.warnings, vec!["steam: not found".to_string()]);
}

#[test]
fn library_folders_old_and_new_formats() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("steamapps")).unwrap();
    std::fs::write(
        root.join("steamapps/libraryfolders.vdf"),
        "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\"1\"\n\t\"ContentStatsID\"\t\"2\"\n\t\"1\"\t\"/old/format\"\n\t\"2\"\n\t{\n\t\t\"path\"\t\"/new/format\"\n\t}\n}\n",
    )
    .unwrap();
    let dirs = grapple_scan::steam_library_dirs(root);
    assert_eq!(dirs, vec![root.to_path_buf(), PathBuf::from("/old/format"), PathBuf::from("/new/format")]);
}

#[test]
fn steam_state_flags() {
    use grapple_scan::steam_status_from_flags as st;
    assert_eq!(st(4), None); // fully installed
    assert_eq!(st(6), Some(GameStatus::UpdateQueued)); // installed + update required
    assert_eq!(st(4 | 2 | 512), Some(GameStatus::UpdateQueued)); // paused
    assert_eq!(st(4 | 2 | 1024), Some(GameStatus::Updating)); // update started
    assert_eq!(st(4 | 2 | 256), Some(GameStatus::Updating)); // update running
    assert_eq!(st(4 | 0x10_0000), Some(GameStatus::Updating)); // downloading
    assert_eq!(st(1026 | 0x20_0000), Some(GameStatus::Updating)); // staging
    assert_eq!(st(1), None); // uninstalled
}

fn build_epic(tmp: &Path) -> (PathBuf, PathBuf) {
    let heroic = tmp.join("heroic");
    copy_fixture(&fixtures().join("heroic"), &heroic, tmp);
    std::fs::create_dir_all(tmp.join("games/rocketleague")).unwrap();
    let images = heroic.join("images-cache");
    // Art files named after sha256(url [+ suffix]) exactly as Heroic does.
    touch(&images.join(sha("https://cdn.example.test/item/Tall_1200x1600-aaa?h=400&resize=1&w=300")));
    touch(&images.join(sha("https://cdn.example.test/item/Wide_2560x1440-bbb")));
    touch(&images.join(sha("https://cdn.example.test/item/FG_Tall-ddd"))); // raw-URL cover, no thumbnail suffix
    touch(&images.join("unrelated-file"));
    let leg = heroic.join("legendaryConfig/legendary");
    (heroic, leg)
}

#[test]
fn heroic_cache_names_are_sha256_of_the_url() {
    // Known answer taken from Josh's real Heroic cache (file name in images-cache for this URL).
    let url = "https://cdn1.epicgames.com/item/9773aa1aa54f4f7b80e44bef04986cea/Rocket_Wide_2560x1440-b102a5ee64c64a9199673ad09fae6e42";
    assert_eq!(sha(url), "4cfddfea10e36e1def0bb5f444ef1fe95f82922c1652e2609b15a3594cc82dda");
    assert_eq!(
        sha(&format!("{}?h=400&resize=1&w=300", url.replace("Rocket_Wide_2560x1440-b102a5ee64c64a9199673ad09fae6e42", "RocketTall_1200x1600-fab62ea387aa4da2a1e67835db3bb300"))),
        "405f0ac4203f8d22fa8051aad3eb2a77d4bde48c00a331d9b7a27dea9f37740d"
    );
}

#[test]
fn epic_scan_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let (heroic, leg) = build_epic(tmp.path());
    let r = scan_all(&ScanConfig { steam_root: None, heroic_config: Some(heroic.clone()), legendary_dir: Some(leg), egl_dir: None });

    let ids: Vec<_> = r.games.iter().map(|g| g.id.0.as_str()).collect();
    // Fall Guys (owned), Gone Game (installed.json but folder missing), Rocket League. DLC and content packs are dropped.
    assert_eq!(ids, ["epic:0a2d9f6403244d12969e11da6713137b", "epic:Gone", "epic:Sugar"]);
    // One warning for broken.json and one for the missing steam root; the scan carried on.
    assert!(r.warnings.iter().any(|w| w.contains("broken.json")), "{:?}", r.warnings);

    let rl = game(&r, "epic:Sugar");
    assert_eq!(rl.store, Store::Epic);
    assert_eq!(rl.name, "Rocket League®");
    assert_eq!(rl.sort_name, "rocket league");
    assert!(rl.installed);
    assert_eq!(rl.install_dir.as_deref(), Some(tmp.path().join("games/rocketleague").as_path()));
    assert_eq!(rl.size_bytes, Some(4368));
    assert_eq!(rl.store_playtime_min, 4760);
    assert_eq!(rl.last_played, Some(1_792_278_682)); // 2026-10-17T23:11:22Z
    let images = heroic.join("images-cache");
    assert_eq!(rl.art.cover, Some(images.join(sha("https://cdn.example.test/item/Tall_1200x1600-aaa?h=400&resize=1&w=300"))));
    assert_eq!(rl.art.hero, Some(images.join(sha("https://cdn.example.test/item/Wide_2560x1440-bbb"))));
    assert_eq!(rl.art.logo, None, "the logo file is not in the cache, so no path");
    assert_eq!(rl.art.icon, None);

    let fg = game(&r, "epic:0a2d9f6403244d12969e11da6713137b");
    assert_eq!(fg.name, "Fall Guys");
    assert!(!fg.installed);
    assert_eq!(fg.store_playtime_min, 0);
    assert_eq!(fg.art.cover, Some(images.join(sha("https://cdn.example.test/item/FG_Tall-ddd"))), "falls back to the raw URL");
    assert_eq!(fg.art.hero, None);

    let gone = game(&r, "epic:Gone");
    assert_eq!(gone.name, "Gone Game (metadata title)");
    assert!(!gone.installed);
    assert_eq!(gone.install_dir, None);
    assert_eq!(gone.size_bytes, None);
    assert_eq!(gone.store_playtime_min, 3);
    assert_eq!(gone.last_played, None, "unparseable date is ignored");
}

#[test]
fn epic_without_heroic_cache_still_lists_games_without_art() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, leg) = build_epic(tmp.path());
    let r = scan_all(&ScanConfig { steam_root: None, heroic_config: None, legendary_dir: Some(leg), egl_dir: None });
    let rl = game(&r, "epic:Sugar");
    assert!(rl.installed);
    assert_eq!(rl.art, Default::default());
    assert_eq!(rl.store_playtime_min, 0);
}

#[test]
fn steam_and_epic_together_keep_store_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = build_steam(tmp.path());
    let (heroic, leg) = build_epic(tmp.path());
    let r = scan_all(&ScanConfig { steam_root: Some(root), heroic_config: Some(heroic), legendary_dir: Some(leg), egl_dir: None });
    assert_eq!(r.games.len(), 8);
    assert!(r.games[..5].iter().all(|g| g.store == Store::Steam));
    assert!(r.games[5..].iter().all(|g| g.store == Store::Epic));
    // Ids are unique across stores.
    let mut ids: Vec<_> = r.games.iter().map(|g| g.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8);
}
