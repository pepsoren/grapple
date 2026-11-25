//! Epic games as installed by Heroic (legendary's files). Read-only; `user.json` is never opened.
//!
//! * `legendary/installed.json`  installed games (title, install_path, install_size)
//! * `legendary/metadata/*.json` every owned game's title and key-art URLs (also DLC, which is skipped)
//! * `heroic/images-cache/<sha256(url)>` Heroic's own downloaded art. Heroic names the file after the SHA-256
//!   of the exact URL it requested: the raw URL for wide art, `url?h=400&resize=1&w=300` for the portrait
//!   thumbnail. We only point at files that exist; nothing is downloaded or copied.
//! * `heroic/store/timestamp.json` Heroic's playtime: `{ "<app_name>": { totalPlayed: minutes, lastPlayed: ISO } }`

use grapple_core::{Art, Game, GameId, Store, sort_name};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const COVER_SUFFIXES: &[&str] = &["?h=400&resize=1&w=300", ""];
const HERO_SUFFIXES: &[&str] = &[""];

pub fn scan(legendary: &Path, heroic: Option<&Path>, warnings: &mut Vec<String>) -> Vec<Game> {
    let installed = read_json(&legendary.join("installed.json"), warnings);
    let installed = match installed {
        Some(Value::Object(m)) => m,
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| Some((v.get("app_name")?.as_str()?.to_string(), v)))
            .collect(),
        _ => serde_json::Map::new(),
    };

    let mut metadata: HashMap<String, Value> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(legendary.join("metadata")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Some(v) = read_json(&p, warnings)
                && let Some(app) = v.get("app_name").and_then(Value::as_str) {
                    metadata.insert(app.to_string(), v);
                }
        }
    }

    let timestamps = heroic
        .and_then(|h| read_json(&h.join("store").join("timestamp.json"), warnings))
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    let images = heroic.map(|h| h.join("images-cache"));

    let mut apps: Vec<&String> = installed.keys().chain(metadata.keys()).collect();
    apps.sort();
    apps.dedup();

    let mut games = Vec::new();
    for app in apps {
        let inst = installed.get(app);
        let meta = metadata.get(app);
        if inst.and_then(|i| i.get("is_dlc")).and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        // An owned-but-not-installed game must look like a game, not DLC or a hidden content pack.
        if inst.is_none() && !meta.is_some_and(is_base_game) {
            continue;
        }
        let name = meta
            .and_then(|m| m.get("app_title").and_then(Value::as_str))
            .or_else(|| meta.and_then(|m| m.pointer("/metadata/title").and_then(Value::as_str)))
            .or_else(|| inst.and_then(|i| i.get("title").and_then(Value::as_str)))
            .unwrap_or(app)
            .to_string();

        let install_dir = inst.and_then(|i| i.get("install_path").and_then(Value::as_str)).map(PathBuf::from);
        // Installed according to legendary, but the folder is gone (unplugged drive): show as not installed.
        let on_disk = install_dir.as_ref().is_some_and(|p| p.exists());

        let ts = timestamps.get(app.as_str());
        let store_playtime_min = ts.and_then(|t| t.get("totalPlayed")).and_then(Value::as_u64).unwrap_or(0);
        let last_played = ts
            .and_then(|t| t.get("lastPlayed"))
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp());

        let art = match (&images, meta) {
            (Some(dir), Some(m)) => epic_art(dir, m),
            _ => Art::default(),
        };

        games.push(Game {
            id: GameId::new(Store::Epic, app),
            store: Store::Epic,
            sort_name: sort_name(&name),
            name,
            installed: inst.is_some() && on_disk,
            install_dir: if on_disk { install_dir } else { None },
            size_bytes: if on_disk { inst.and_then(|i| i.get("install_size")).and_then(Value::as_u64) } else { None },
            art,
            store_playtime_min,
            last_played,
            status: None,
        });
    }
    games.sort_by(|a, b| a.sort_name.cmp(&b.sort_name).then_with(|| a.id.cmp(&b.id)));
    games
}

/// Add the games the Epic Games Launcher installed (Windows). A game legendary already has installed stays as
/// legendary sees it; one legendary only owns becomes installed in the launcher's folder; the rest are added with
/// the launcher's name for them (no art: Heroic's image cache is the only art source here).
pub fn add_egl(games: &mut Vec<Game>, egl: Vec<grapple_core::egl::EglGame>) {
    for e in egl {
        let id = GameId::new(Store::Epic, &e.app_name);
        let on_disk = e.install_location.exists();
        match games.iter_mut().find(|g| g.id == id) {
            Some(g) if g.installed => {}
            Some(g) => {
                g.installed = on_disk;
                g.install_dir = on_disk.then(|| e.install_location.clone());
                g.size_bytes = if on_disk { e.install_size } else { None };
            }
            None => games.push(Game {
                id,
                store: Store::Epic,
                sort_name: sort_name(&e.display_name),
                name: e.display_name.clone(),
                installed: on_disk,
                install_dir: on_disk.then(|| e.install_location.clone()),
                size_bytes: if on_disk { e.install_size } else { None },
                art: Art::default(),
                store_playtime_min: 0,
                last_played: None,
                status: None,
            }),
        }
    }
    games.sort_by(|a, b| a.sort_name.cmp(&b.sort_name).then_with(|| a.id.cmp(&b.id)));
}

fn read_json(path: &Path, warnings: &mut Vec<String>) -> Option<Value> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warnings.push(format!("epic: cannot read {}: {e}", path.display()));
            return None;
        }
    };
    match serde_json::from_slice(&data) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("epic: {} is not valid JSON: {e}", path.display()));
            None
        }
    }
}

/// Base games have a `games` category and no parent item; DLC and content packs have a parent or only `hidden`/`addons`.
fn is_base_game(meta: &Value) -> bool {
    let md = &meta["metadata"];
    let has_parent = md.get("mainGameItemList").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
    let is_game_category = md
        .get("categories")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().any(|x| matches!(x.get("path").and_then(Value::as_str), Some("games" | "applications"))));
    !has_parent && is_game_category
}

fn cache_file(dir: &Path, url: &str, suffixes: &[&str]) -> Option<PathBuf> {
    for suffix in suffixes {
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        h.update(suffix.as_bytes());
        let name = hex(&h.finalize());
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

fn epic_art(images: &Path, meta: &Value) -> Art {
    let mut urls: HashMap<&str, &str> = HashMap::new();
    if let Some(list) = meta.pointer("/metadata/keyImages").and_then(Value::as_array) {
        for img in list {
            if let (Some(t), Some(u)) = (img.get("type").and_then(Value::as_str), img.get("url").and_then(Value::as_str)) {
                urls.entry(t).or_insert(u);
            }
        }
    }
    let find = |types: &[&str], suffixes: &[&str]| -> Option<PathBuf> {
        types.iter().find_map(|t| urls.get(t).and_then(|u| cache_file(images, u, suffixes)))
    };
    Art {
        cover: find(&["DieselGameBoxTall", "OfferImageTall"], COVER_SUFFIXES),
        hero: find(&["DieselGameBox", "OfferImageWide"], HERO_SUFFIXES),
        logo: find(&["DieselGameBoxLogo"], HERO_SUFFIXES),
        icon: None,
    }
}

#[cfg(test)]
mod egl_tests {
    use super::*;
    use grapple_core::egl::EglGame;

    fn game(app: &str, installed: bool) -> Game {
        Game {
            id: GameId::new(Store::Epic, app),
            store: Store::Epic,
            name: app.into(),
            sort_name: sort_name(app),
            installed,
            install_dir: installed.then(|| PathBuf::from("/legendary").join(app)),
            size_bytes: None,
            art: Art::default(),
            store_playtime_min: 7,
            last_played: None,
            status: None,
        }
    }

    #[test]
    fn launcher_installs_join_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let egl = |app: &str, name: &str| EglGame {
            app_name: app.into(),
            display_name: name.into(),
            install_location: dir.path().join(app),
            install_size: Some(10),
            ..EglGame::default()
        };
        for app in ["Both", "Owned", "New"] {
            std::fs::create_dir_all(dir.path().join(app)).unwrap();
        }
        let mut games = vec![game("Both", true), game("Owned", false)];
        add_egl(&mut games, vec![egl("Both", "Both"), egl("Owned", "Owned"), egl("New", "A New Game"), egl("Gone", "Gone")]);
        let by = |app: &str| games.iter().find(|g| g.id == GameId::new(Store::Epic, app)).unwrap();
        assert_eq!(by("Both").install_dir, Some(PathBuf::from("/legendary/Both")), "legendary's install is kept");
        assert!(by("Owned").installed);
        assert_eq!(by("Owned").install_dir, Some(dir.path().join("Owned")));
        assert_eq!(by("Owned").store_playtime_min, 7, "what legendary/Heroic knew stays");
        assert_eq!(by("New").name, "A New Game");
        assert!(by("New").installed && by("New").size_bytes == Some(10));
        assert!(!by("Gone").installed, "a folder that is gone is not installed");
        assert_eq!(games.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["A New Game", "Both", "Gone", "Owned"]);
    }
}
