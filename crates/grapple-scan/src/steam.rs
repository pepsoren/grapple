//! Steam: names/types from `appcache/appinfo.vdf`, installs from `appmanifest_*.acf`, playtime from
//! `userdata/<id>/config/localconfig.vdf`, art from `appcache/librarycache`. Read-only, no login.

use crate::appinfo::{self, AppMeta};
use crate::vdf::Vdf;
use grapple_core::{Art, Game, GameId, GameStatus, Store, sort_name};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// `StateFlags` bits of an `appmanifest_*.acf` (Steam's `EAppState`).
pub mod flags {
    pub const UPDATE_REQUIRED: u64 = 1 << 1; // 2
    pub const FULLY_INSTALLED: u64 = 1 << 2; // 4
    pub const UPDATE_RUNNING: u64 = 1 << 8; // 256
    pub const UPDATE_PAUSED: u64 = 1 << 9; // 512
    pub const UPDATE_STARTED: u64 = 1 << 10; // 1024
    pub const DOWNLOADING: u64 = 1 << 20; // 0x100000
    pub const STAGING: u64 = 1 << 21; // 0x200000
    pub const COMMITTING: u64 = 1 << 22; // 0x400000
    pub const ACTIVE_UPDATE: u64 = UPDATE_RUNNING | UPDATE_STARTED | DOWNLOADING | STAGING | COMMITTING;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub appid: u32,
    pub name: String,
    pub install_dir: PathBuf,
    pub state_flags: u64,
    pub size_on_disk: Option<u64>,
    pub last_played: Option<i64>,
}

impl Manifest {
    pub fn installed(&self) -> bool {
        self.state_flags & flags::FULLY_INSTALLED != 0
    }

    pub fn status(&self) -> Option<GameStatus> {
        status_from_flags(self.state_flags)
    }
}

/// Steam shows "Update queued" for an installed game that needs an update but is not downloading yet
/// (flags 6 = installed + update required, or a paused update) and "Updating" while a download, staging
/// or commit is under way.
pub fn status_from_flags(f: u64) -> Option<GameStatus> {
    if f & flags::ACTIVE_UPDATE != 0 {
        Some(GameStatus::Updating)
    } else if f & (flags::UPDATE_REQUIRED | flags::UPDATE_PAUSED) != 0 {
        Some(GameStatus::UpdateQueued)
    } else {
        None
    }
}

pub fn parse_manifest(text: &str, libdir: &Path) -> Option<Manifest> {
    let v = Vdf::parse(text);
    let st = v.get("AppState")?;
    let appid = st.u64_at(&["appid"])? as u32;
    let installdir = st.str_at(&["installdir"]).unwrap_or_default();
    Some(Manifest {
        appid,
        name: st.str_at(&["name"]).unwrap_or_default().to_string(),
        install_dir: libdir.join("steamapps").join("common").join(installdir),
        state_flags: st.u64_at(&["StateFlags"]).unwrap_or(0),
        size_on_disk: st.u64_at(&["SizeOnDisk"]),
        last_played: st.u64_at(&["LastPlayed"]).filter(|&t| t > 0).map(|t| t as i64),
    })
}

/// All library roots: the Steam root itself plus every `path` in `libraryfolders.vdf`.
pub fn library_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    let text = ["steamapps", "config"]
        .iter()
        .find_map(|d| std::fs::read_to_string(root.join(d).join("libraryfolders.vdf")).ok());
    if let Some(text) = text {
        let v = Vdf::parse(&text);
        if let Some(lf) = v.get("libraryfolders") {
            for (key, val) in lf.entries() {
                if !key.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                // Current format: "0" { "path" "/x" ... }; old format: "1" "/x".
                let path = match val {
                    Vdf::Obj(_) => val.str_at(&["path"]),
                    Vdf::Str(s) => Some(s.as_str()),
                };
                if let Some(p) = path.filter(|p| !p.is_empty()) {
                    let p = PathBuf::from(p);
                    if !dirs.contains(&p) {
                        dirs.push(p);
                    }
                }
            }
        }
    }
    dirs
}

fn read_manifests(libs: &[PathBuf]) -> Vec<Manifest> {
    let mut out: HashMap<u32, Manifest> = HashMap::new();
    for lib in libs {
        let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            if let Some(m) = parse_manifest(&text, lib) {
                out.entry(m.appid).or_insert(m);
            }
        }
    }
    out.into_values().collect()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LocalPlay {
    pub playtime_min: u64,
    pub last_played: Option<i64>,
}

/// Playtime per appid from the most recently used account in `userdata/`.
pub fn read_local_config(root: &Path) -> HashMap<u32, LocalPlay> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(root.join("userdata")) {
        for e in rd.flatten() {
            let p = e.path().join("config").join("localconfig.vdf");
            if let Ok(mt) = std::fs::metadata(&p).and_then(|m| m.modified())
                && best.as_ref().is_none_or(|(t, _)| mt > *t) {
                    best = Some((mt, p));
                }
        }
    }
    let mut out = HashMap::new();
    let Some((_, path)) = best else { return out };
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    let v = Vdf::parse(&text);
    if let Some(apps) = v.path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"]) {
        for (k, app) in apps.entries() {
            let Ok(id) = k.parse::<u32>() else { continue };
            out.insert(
                id,
                LocalPlay {
                    playtime_min: app.u64_at(&["Playtime"]).unwrap_or(0),
                    last_played: app.u64_at(&["LastPlayed"]).filter(|&t| t > 0).map(|t| t as i64),
                },
            );
        }
    }
    out
}

/// Tools and runtimes that show up as installed apps. Only used when appinfo has no entry for an app.
fn looks_like_tool(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("proton")
        || n.starts_with("steam linux runtime")
        || n.starts_with("steamworks common")
        || n.starts_with("steamvr")
        || n == "steam controller configs"
}

fn is_hex_icon(name: &str) -> bool {
    name.len() == 44 && name.ends_with(".jpg") && name.as_bytes()[..40].iter().all(u8::is_ascii_hexdigit)
}

/// Find a game's art files in `appcache/librarycache/<appid>/`. Files sit either in the folder root or in
/// 40-hex-named subfolders; this only lists directories, it never opens an image.
pub fn find_art(dir: &Path, icon_hint: Option<&str>) -> Art {
    #[derive(Default)]
    struct Found {
        cover: Option<(u8, PathBuf)>,
        hero: Option<PathBuf>,
        logo: Option<PathBuf>,
        icon: Option<(bool, PathBuf)>,
    }
    fn visit(f: &mut Found, name: &str, path: PathBuf, hint: Option<&str>) {
        match name {
            "library_600x900.jpg" => {
                if f.cover.as_ref().is_none_or(|(p, _)| *p > 0) {
                    f.cover = Some((0, path));
                }
            }
            "library_capsule.jpg" => {
                if f.cover.is_none() {
                    f.cover = Some((1, path));
                }
            }
            "library_hero.jpg" => {
                f.hero.get_or_insert(path);
            }
            "logo.png" => {
                f.logo.get_or_insert(path);
            }
            n if is_hex_icon(n) => {
                let hit = hint.is_some_and(|h| n[..40].eq_ignore_ascii_case(h));
                if f.icon.as_ref().is_none_or(|(was_hit, _)| !was_hit && hit) {
                    f.icon = Some((hit, path));
                }
            }
            _ => {}
        }
    }

    let mut f = Found::default();
    let mut subdirs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if ft.is_dir() {
                subdirs.push(e.path());
            } else {
                visit(&mut f, name, e.path(), icon_hint);
            }
        }
    }
    // Root files win over hashed subfolders because they were visited first.
    for sub in subdirs {
        let Ok(rd) = std::fs::read_dir(&sub) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            if let Some(name) = name.to_str() {
                visit(&mut f, name, e.path(), icon_hint);
            }
        }
    }
    Art { cover: f.cover.map(|c| c.1), hero: f.hero, logo: f.logo, icon: f.icon.map(|i| i.1) }
}

pub fn scan(root: &Path, warnings: &mut Vec<String>) -> Vec<Game> {
    let libs = library_dirs(root);
    let manifests = read_manifests(&libs);
    let manifest_by_id: HashMap<u32, &Manifest> = manifests.iter().map(|m| (m.appid, m)).collect();

    // Known apps: every numeric folder in the library cache, plus whatever is installed.
    let cache_dir = root.join("appcache").join("librarycache");
    let mut known: HashSet<u32> = manifests.iter().map(|m| m.appid).collect();
    let mut cached: HashSet<u32> = HashSet::new();
    match std::fs::read_dir(&cache_dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                if let Some(id) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                    cached.insert(id);
                    known.insert(id);
                }
            }
        }
        Err(e) => warnings.push(format!("steam: cannot read {}: {e}", cache_dir.display())),
    }

    let metas: Option<HashMap<u32, AppMeta>> = match std::fs::read(root.join("appcache").join("appinfo.vdf")) {
        Ok(data) => match appinfo::parse(&data, |id| known.contains(&id)) {
            Ok(m) => Some(m),
            Err(e) => {
                warnings.push(format!("steam: appinfo.vdf: {e}; listing installed games only"));
                None
            }
        },
        Err(e) => {
            warnings.push(format!("steam: cannot read appinfo.vdf: {e}; listing installed games only"));
            None
        }
    };

    let local = read_local_config(root);
    if local.is_empty() {
        warnings.push("steam: no playtime found (userdata/*/config/localconfig.vdf missing or empty)".into());
    }

    let mut ids: Vec<u32> = known.into_iter().collect();
    ids.sort_unstable();

    let build = |id: u32| -> Option<Game> {
        let man = manifest_by_id.get(&id).copied();
        let meta = metas.as_ref().and_then(|m| m.get(&id));
        let is_game = match meta {
            // Demos are their own app type; Steam lists them in the library like games.
            Some(m) => matches!(m.kind.as_str(), "game" | "demo"),
            // No appinfo entry: only trust an installed app that does not look like a tool.
            None => man.is_some_and(|m| !looks_like_tool(&m.name)),
        };
        if !is_game {
            return None;
        }
        let name = meta
            .and_then(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| man.map(|m| m.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| format!("App {id}"));
        let art = if cached.contains(&id) {
            find_art(&cache_dir.join(id.to_string()), meta.and_then(|m| m.client_icon.as_deref()))
        } else {
            Art::default()
        };
        let lp = local.get(&id).copied().unwrap_or_default();
        let last_played = [lp.last_played, man.and_then(|m| m.last_played)].into_iter().flatten().max();
        Some(Game {
            id: GameId::new(Store::Steam, &id.to_string()),
            store: Store::Steam,
            sort_name: sort_name(&name),
            name,
            installed: man.is_some_and(Manifest::installed),
            install_dir: man.filter(|m| m.installed()).map(|m| m.install_dir.clone()),
            size_bytes: man.and_then(|m| m.size_on_disk),
            art,
            store_playtime_min: lp.playtime_min,
            last_played,
            status: man.and_then(Manifest::status),
        })
    };

    // Art lookup is ~5 directory reads per game; serial it is ~9 ms for 164 games, so no threads.
    let mut games: Vec<Game> = ids.iter().filter_map(|&id| build(id)).collect();
    games.sort_by(|a, b| a.sort_name.cmp(&b.sort_name).then_with(|| a.id.cmp(&b.id)));
    games
}
