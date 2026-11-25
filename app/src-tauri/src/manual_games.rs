//! Manual games ("Add any game"): the list lives in the Db as one setting (`manual.games`), is merged into the
//! game list, and its `ManualEntry`s are handed to the launcher (`LaunchSettings.manual`).
//!
//! Everything here is pure (no Tauri, no disk except `exists` checks in `to_game`), so it is unit tested.

use grapple_core::{Art, Db, Game, GameId, Store, sort_name};
use grapple_launch::heroic::shlex_split;
use grapple_launch::manual::{ManualEntry, ManualKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const SETTING_KEY: &str = "manual.games";
const NAME_MAX: usize = 128;

/// One stored manual game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualGame {
    /// `manual:<uuid>`
    pub id: String,
    pub name: String,
    pub entry: ManualEntry,
    pub added_at: i64,
}

pub use crate::backend::ManualDraft as NewManual;

/// What the picker found out about a file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detected {
    pub name: String,
    pub path: String,
    /// The kind Grapple would use: `windows`, `appimage`, `native` or `command`.
    pub kind: &'static str,
    /// Plain words for the form ("Windows program, runs through Proton").
    pub label: &'static str,
}

pub fn kind_name(k: ManualKind) -> &'static str {
    match k {
        ManualKind::Auto => "auto",
        ManualKind::Native => "native",
        ManualKind::AppImage => "appimage",
        ManualKind::Windows => "windows",
        ManualKind::Command => "command",
    }
}

pub fn parse_kind(s: &str) -> Option<ManualKind> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => ManualKind::Auto,
        "native" => ManualKind::Native,
        "appimage" => ManualKind::AppImage,
        "windows" | "proton" => ManualKind::Windows,
        "command" => ManualKind::Command,
        _ => return None,
    })
}

fn kind_label(k: ManualKind) -> &'static str {
    match k {
        ManualKind::Windows if cfg!(windows) => "Windows program",
        ManualKind::Windows => "Windows program, runs through Proton",
        ManualKind::AppImage => "AppImage",
        ManualKind::Native => "Native program",
        ManualKind::Command => "Custom command",
        ManualKind::Auto => "Detected from the file",
    }
}

/// A readable default name from a file name: `Hollow_Knight-v1.2.exe` -> `Hollow Knight v1.2`.
pub fn default_name(path: &str) -> String {
    let file = Path::new(path.trim_end_matches('/')).file_name().and_then(|n| n.to_str()).unwrap_or(path);
    let lower = file.to_ascii_lowercase();
    let stem = [".exe", ".bat", ".cmd", ".msi", ".appimage", ".x86_64", ".sh"]
        .iter()
        .find(|e| lower.ends_with(**e))
        .map_or(file, |e| &file[..file.len() - e.len()]);
    let spaced: String = stem.chars().map(|c| if c == '_' { ' ' } else { c }).collect();
    let name = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() { file.to_string() } else { name }
}

/// Look at a path the person picked. Pure: only the text of the path is used (a picked file is a file).
pub fn detect(path: &str) -> Detected {
    let probe = ManualEntry { exe: path.to_string(), kind: ManualKind::Auto, ..Default::default() };
    let kind = probe.effective_kind();
    Detected { name: default_name(path), path: path.to_string(), kind: kind_name(kind), label: kind_label(kind) }
}

/// Validate the form and make the stored game. `id` is `manual:<uuid>`.
pub fn build(new: &NewManual, id: String, now: i64) -> Result<ManualGame, String> {
    let name = new.name.trim();
    if name.is_empty() {
        return Err("Give the game a name".into());
    }
    if name.chars().count() > NAME_MAX {
        return Err(format!("Keep the name under {NAME_MAX} characters"));
    }
    let path = new.path.trim();
    if path.is_empty() {
        return Err("Choose the program to run".into());
    }
    let kind = parse_kind(&new.kind).ok_or_else(|| format!("Unknown kind: {}", new.kind))?;
    let mut entry = ManualEntry { exe: path.to_string(), kind, ..Default::default() };
    // Windows programs need a real file; a command is looked up on PATH; native/AppImage need a path.
    let eff = entry.effective_kind();
    if eff != ManualKind::Command && !full_path(path) {
        return Err("Use the full path of the program (or pick it with Choose file)".into());
    }
    if path.contains('\0') {
        return Err("That path is not valid".into());
    }
    entry.kind = eff; // store what was decided, so a later rename of the file cannot change how it starts
    let args = new.args.trim();
    if !args.is_empty() {
        entry.args = shlex_split(args);
    }
    Ok(ManualGame { id, name: name.to_string(), entry, added_at: now })
}

/// A full path: `/...` on Linux; on Windows anything from a root (`C:\...`, `\\server\share`, or `\...` on the
/// current drive), not a relative or drive-relative (`C:x`) one.
fn full_path(path: &str) -> bool {
    if cfg!(windows) { Path::new(path).has_root() } else { path.starts_with('/') }
}

pub fn new_id() -> String {
    format!("manual:{}", uuid::Uuid::new_v4())
}

pub fn game_id(m: &ManualGame) -> GameId {
    GameId(m.id.clone())
}

/// The `Game` the rest of the app sees. `installed` is whether the program still exists (always true for a
/// command, which is looked up on `PATH` when it starts).
pub fn to_game(m: &ManualGame) -> Game {
    let installed = m.entry.effective_kind() == ManualKind::Command || Path::new(&m.entry.exe).exists();
    let dir = Path::new(&m.entry.exe).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
    Game {
        id: game_id(m),
        store: Store::Manual,
        name: m.name.clone(),
        sort_name: sort_name(&m.name),
        installed,
        install_dir: dir,
        size_bytes: None,
        art: Art::default(),
        store_playtime_min: 0,
        last_played: None,
        status: None,
    }
}

/// Scanned games plus the manual ones, sorted like the Db sorts (`sort_name`, then id). A manual id that a
/// scan result already holds (the Db keeps old manual rows) is not listed twice.
pub fn merge(mut games: Vec<Game>, manual: &[ManualGame]) -> Vec<Game> {
    for m in manual {
        if let Some(i) = games.iter().position(|g| g.id.0 == m.id) {
            games.remove(i);
        }
        games.push(to_game(m));
    }
    games.sort_by(|a, b| a.sort_name.cmp(&b.sort_name).then_with(|| a.id.0.cmp(&b.id.0)));
    games
}

/// `LaunchSettings.manual`.
pub fn launch_map(manual: &[ManualGame]) -> HashMap<GameId, ManualEntry> {
    manual.iter().map(|m| (game_id(m), m.entry.clone())).collect()
}

/// Add (or replace, by id).
pub fn add(list: &mut Vec<ManualGame>, g: ManualGame) {
    match list.iter_mut().find(|x| x.id == g.id) {
        Some(slot) => *slot = g,
        None => list.push(g),
    }
}

/// Remove by id; `true` if it was there.
pub fn remove(list: &mut Vec<ManualGame>, id: &str) -> bool {
    let before = list.len();
    list.retain(|g| g.id != id);
    list.len() != before
}

/// The stored list as read: the games that parsed, plus whatever could not be read, kept so a save puts it back.
#[derive(Debug, Default)]
pub struct ManualList {
    pub games: Vec<ManualGame>,
    /// Array entries that did not deserialize (e.g. written by a newer version); written back unchanged.
    unreadable: Vec<serde_json::Value>,
    /// The raw text when the setting was not a JSON array at all; backed up before a save overwrites it.
    broken: Option<String>,
}

const BROKEN_KEY: &str = "manual.games.broken";

pub fn load_list(db: &Db) -> ManualList {
    let raw = match db.setting_raw(SETTING_KEY) {
        Ok(Some(raw)) => raw,
        Ok(None) => return ManualList::default(),
        Err(e) => {
            tracing::warn!("manual games: cannot read the setting: {e}");
            return ManualList::default();
        }
    };
    let items = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Array(items)) => items,
        other => {
            let why = match other {
                Err(e) => e.to_string(),
                _ => "not a JSON array".to_string(),
            };
            tracing::warn!("manual games: the stored list is unreadable ({why}); it is kept under {BROKEN_KEY} if it is overwritten");
            return ManualList { broken: Some(raw), ..ManualList::default() };
        }
    };
    let mut list = ManualList::default();
    for item in items {
        match serde_json::from_value::<ManualGame>(item.clone()) {
            Ok(g) => list.games.push(g),
            Err(e) => {
                let field = |k: &str| item.get(k).and_then(|v| v.as_str()).unwrap_or("?").to_string();
                tracing::warn!("manual games: skipping entry id={} name={}: {e}", field("id"), field("name"));
                list.unreadable.push(item);
            }
        }
    }
    list
}

/// Write the games, then the entries that could not be read, unchanged. A corrupt setting is copied to
/// `manual.games.broken` (or `.broken.<unix secs>` if that is taken) first.
pub fn save_list(db: &Db, list: &ManualList) -> Result<(), grapple_core::db::Error> {
    if let Some(raw) = &list.broken {
        let mut key = BROKEN_KEY.to_string();
        if db.setting_raw(&key)?.is_some() {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            key = format!("{BROKEN_KEY}.{secs}");
        }
        // a stored string is JSON text, so store the raw text as a JSON string value
        db.set_setting(&key, raw)?;
    }
    let mut all: Vec<serde_json::Value> = Vec::with_capacity(list.games.len() + list.unreadable.len());
    for g in &list.games {
        all.push(serde_json::to_value(g)?);
    }
    all.extend(list.unreadable.iter().cloned());
    db.set_setting(SETTING_KEY, &all)
}

/// The games that could be read (read-only call sites).
pub fn load(db: &Db) -> Vec<ManualGame> {
    load_list(db).games
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(name: &str, path: &str, kind: &str, args: &str) -> NewManual {
        NewManual { name: name.into(), path: path.into(), kind: kind.into(), args: args.into() }
    }

    #[test]
    fn default_names_come_from_the_file() {
        assert_eq!(default_name("/games/Hollow_Knight.exe"), "Hollow Knight");
        assert_eq!(default_name("/opt/Cool-Game.AppImage"), "Cool-Game");
        assert_eq!(default_name("/opt/run.sh"), "run");
        assert_eq!(default_name("/opt/ryujinx"), "ryujinx");
        assert_eq!(default_name("/opt/a__b.x86_64"), "a b");
    }

    #[test]
    fn detects_the_kind_from_the_path() {
        let d = detect("/games/Hades/Hades.EXE");
        assert_eq!((d.kind, d.name.as_str()), ("windows", "Hades"));
        assert_eq!(detect("/opt/x.AppImage").kind, "appimage");
        assert_eq!(detect("/opt/x/game").kind, "native");
        assert_eq!(detect("retroarch").kind, "command");
    }

    #[test]
    fn build_validates_and_splits_args() {
        let g = build(&new("  Hades ", "/g/Hades.exe", "auto", r#"-windowed "a b""#), "manual:1".into(), 5).unwrap();
        assert_eq!(g.name, "Hades");
        assert_eq!(g.entry.kind, ManualKind::Windows, "the decided kind is stored");
        assert_eq!(g.entry.args, ["-windowed", "a b"]);
        assert_eq!(g.added_at, 5);

        assert!(build(&new("", "/g/x", "", ""), "manual:1".into(), 0).is_err(), "name required");
        assert!(build(&new("x", "", "", ""), "manual:1".into(), 0).is_err(), "path required");
        assert!(build(&new("x", "relative/game", "", ""), "manual:1".into(), 0).is_err(), "files need a full path");
        assert!(build(&new("x", "retroarch", "command", "-L core"), "manual:1".into(), 0).is_ok(), "a command may be bare");
        assert!(build(&new("x", "/g/x", "bogus", ""), "manual:1".into(), 0).is_err());
        assert!(build(&new(&"n".repeat(200), "/g/x", "", ""), "manual:1".into(), 0).is_err());
        // an explicit kind wins over the file name
        assert_eq!(build(&new("x", "/g/x.exe", "native", ""), "manual:1".into(), 0).unwrap().entry.kind, ManualKind::Native);
    }

    #[test]
    fn ids_are_manual_uuids() {
        let (a, b) = (new_id(), new_id());
        assert!(a.starts_with("manual:") && a.len() > "manual:".len() && a != b);
        assert!(crate::backend::valid_game_id(&a), "the art protocol accepts it");
    }

    #[test]
    fn to_game_marks_missing_programs_not_installed() {
        let mut m = build(&new("Tool", "/definitely/not/here/tool", "", ""), "manual:t".into(), 0).unwrap();
        let g = to_game(&m);
        assert_eq!((g.store, g.installed, g.sort_name.as_str()), (Store::Manual, false, "tool"));
        m.entry = ManualEntry { exe: "retroarch".into(), kind: ManualKind::Command, ..Default::default() };
        assert!(to_game(&m).installed, "a command is looked up on PATH at launch");
        m.entry.exe = "/bin/sh".into();
        m.entry.kind = ManualKind::Native;
        assert!(to_game(&m).installed);
    }

    #[test]
    fn merge_sorts_dedupes_and_maps_launch_entries() {
        let steam = |id: &str, name: &str| Game { id: GameId(format!("steam:{id}")), store: Store::Steam, name: name.into(), sort_name: sort_name(name), ..to_game(&build(&new("x", "cmd", "command", ""), "manual:z".into(), 0).unwrap()) };
        let a = build(&new("Banana", "/b", "native", ""), "manual:a".into(), 0).unwrap();
        let c = build(&new("Apple", "/a", "native", ""), "manual:c".into(), 0).unwrap();
        let merged = merge(vec![steam("1", "Cherry"), steam("2", "Aardvark")], &[a.clone(), c.clone()]);
        let names: Vec<_> = merged.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Aardvark", "Apple", "Banana", "Cherry"]);
        // an old manual row in the scan cache is replaced, not duplicated
        let again = merge(merged.clone(), &[a.clone()]);
        assert_eq!(again.iter().filter(|g| g.id.0 == "manual:a").count(), 1);
        let map = launch_map(&[a, c]);
        assert_eq!(map.len(), 2);
        assert_eq!(map[&GameId("manual:a".into())].exe, "/b");
    }

    #[test]
    fn add_replaces_by_id_and_remove_reports() {
        let mk = |n: &str| build(&new(n, "/p", "native", ""), "manual:1".into(), 0).unwrap();
        let mut list = Vec::new();
        add(&mut list, mk("One"));
        add(&mut list, mk("Renamed"));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Renamed");
        assert!(remove(&mut list, "manual:1"));
        assert!(!remove(&mut list, "manual:1"));
        assert!(list.is_empty());
    }

    #[test]
    fn the_list_round_trips_through_the_db() {
        let db = Db::open_in_memory().unwrap();
        assert!(load(&db).is_empty());
        let g = build(&new("Hades", "/g/Hades.exe", "", "-x"), "manual:9".into(), 7).unwrap();
        let mut list = load_list(&db);
        add(&mut list.games, g.clone());
        save_list(&db, &list).unwrap();
        assert_eq!(load(&db), vec![g]);
        save_list(&db, &ManualList::default()).unwrap();
        assert!(load(&db).is_empty());
    }

    fn two_good_one_bad(db: &Db) -> (Vec<ManualGame>, serde_json::Value) {
        let mk = |id: &str, n: &str| build(&new(n, "/g/x", "native", ""), id.into(), 1).unwrap();
        let (a, b) = (mk("manual:a", "A"), mk("manual:b", "B"));
        let bad = serde_json::json!({"id": "manual:bad", "name": "Bad", "added_at": 3,
            "entry": {"exe": "/x", "kind": "from_the_future"}, "extra": [1, 2]});
        db.set_setting(SETTING_KEY, &serde_json::json!([a, bad, b])).unwrap();
        (vec![a, b], bad)
    }

    #[test]
    fn a_bad_entry_is_skipped_not_fatal() {
        let db = Db::open_in_memory().unwrap();
        let (good, _) = two_good_one_bad(&db);
        assert_eq!(load(&db), good);
    }

    #[test]
    fn adding_keeps_an_unreadable_entry() {
        let db = Db::open_in_memory().unwrap();
        let (good, bad) = two_good_one_bad(&db);
        let new_game = build(&new("C", "/g/c", "native", ""), "manual:c".into(), 9).unwrap();
        let mut list = load_list(&db);
        add(&mut list.games, new_game.clone());
        save_list(&db, &list).unwrap();
        let stored: Vec<serde_json::Value> = db.setting(SETTING_KEY).unwrap().unwrap();
        assert!(stored.contains(&bad), "the bad entry survives unchanged");
        assert_eq!(stored.len(), 4);
        let mut expect = good;
        expect.push(new_game);
        assert_eq!(load(&db), expect);
    }

    #[test]
    fn removing_keeps_an_unreadable_entry() {
        let db = Db::open_in_memory().unwrap();
        let (_, bad) = two_good_one_bad(&db);
        let mut list = load_list(&db);
        assert!(remove(&mut list.games, "manual:a"));
        save_list(&db, &list).unwrap();
        let stored: Vec<serde_json::Value> = db.setting(SETTING_KEY).unwrap().unwrap();
        assert!(stored.contains(&bad));
        assert_eq!(load(&db).iter().map(|g| g.id.as_str()).collect::<Vec<_>>(), ["manual:b"]);
    }

    #[test]
    fn a_corrupt_setting_is_backed_up_before_it_is_overwritten() {
        for garbage in [r#"{"not":"an array"}"#, "this is not json {"] {
            let db = Db::open_in_memory().unwrap();
            // write the raw text directly, JSON or not
            db.set_setting_raw(SETTING_KEY, garbage).unwrap();
            assert!(load(&db).is_empty());
            let mut list = load_list(&db);
            add(&mut list.games, build(&new("C", "/g/c", "native", ""), "manual:c".into(), 9).unwrap());
            save_list(&db, &list).unwrap();
            assert_eq!(load(&db).len(), 1);
            let backup: String = db.setting(BROKEN_KEY).unwrap().unwrap();
            assert_eq!(backup, garbage, "the raw text is kept");
        }
    }

    #[test]
    fn an_existing_backup_is_not_overwritten() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting(BROKEN_KEY, &"older").unwrap();
        db.set_setting_raw(SETTING_KEY, "garbage").unwrap();
        save_list(&db, &load_list(&db)).unwrap();
        assert_eq!(db.setting::<String>(BROKEN_KEY).unwrap().unwrap(), "older");
        assert_eq!(db.setting_raw(SETTING_KEY).unwrap().unwrap(), "[]");
    }
}
