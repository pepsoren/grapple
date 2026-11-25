//! The bridge between the Tauri commands and the Grapple crates.
//!
//! Two interchangeable `Backend`s with the same method set:
//!   * `real`  (feature `real`, the default): grapple-core `Db` + grapple-scan + grapple-launch +
//!                                grapple-tether (the tether service, manual games, now playing).
//!   * `mock`  (feature `mock`, `--no-default-features --features mock`): fixture games from
//!                                mockups/assets and a simulated launch, for UI-only work. MOCK: nothing here
//!                                touches a store, a launcher, tether or a real game.
//!
//! Everything the commands and the art protocol need that is not store-specific lives in this file.

use grapple_core::{Art, Game, GameStatus, PlayStats, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::UNIX_EPOCH;

#[cfg(all(feature = "mock", not(feature = "real")))]
mod mock;
#[cfg(all(feature = "mock", not(feature = "real")))]
mod mock_downloads;
#[cfg(all(feature = "mock", not(feature = "real")))]
pub use mock::Backend;

#[cfg(feature = "real")]
mod real;
#[cfg(feature = "real")]
pub use real::Backend;

#[cfg(not(any(feature = "mock", feature = "real")))]
compile_error!("enable the `mock` or the `real` feature");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtKind {
    Cover,
    Hero,
    Logo,
    Icon,
}

impl ArtKind {
    pub const ALL: [ArtKind; 4] = [ArtKind::Cover, ArtKind::Hero, ArtKind::Logo, ArtKind::Icon];

    pub fn name(self) -> &'static str {
        match self {
            ArtKind::Cover => "cover",
            ArtKind::Hero => "hero",
            ArtKind::Logo => "logo",
            ArtKind::Icon => "icon",
        }
    }

    pub fn parse(s: &str) -> Option<ArtKind> {
        Some(match s {
            "cover" => ArtKind::Cover,
            "hero" => ArtKind::Hero,
            "logo" => ArtKind::Logo,
            "icon" => ArtKind::Icon,
            _ => return None,
        })
    }
}

/// Which art files exist, for the UI to pick fallbacks without a failed request.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ArtFlags {
    pub cover: bool,
    pub hero: bool,
    pub logo: bool,
    pub icon: bool,
}

/// What `list_games` returns: `grapple_core::Game` with art paths turned into flags (the bytes are served
/// at `grapple://art/<id>/<kind>`), plus the favourite flag and the playtime to show.
#[derive(Debug, Clone, Serialize)]
pub struct GameDto {
    pub id: String,
    pub store: Store,
    pub name: String,
    pub sort_name: String,
    pub installed: bool,
    pub size_bytes: Option<u64>,
    pub art: ArtFlags,
    /// Newest mtime among the art files: part of the art URL, so the cache is never stale.
    pub art_v: u64,
    /// max(store total, our sessions), minutes.
    pub playtime_min: u64,
    pub last_played: Option<i64>,
    pub status: Option<GameStatus>,
    pub favourite: bool,
    /// Left out of the lists by the settings ([`Settings::hides`]); the UI still knows it (a hidden game can run).
    pub hidden: bool,
}

impl GameDto {
    pub fn new(g: &Game, favourite: bool, playtime_min: u64, art_v: u64) -> GameDto {
        let flag = |p: &Option<PathBuf>| p.as_deref().is_some_and(Path::exists);
        GameDto {
            id: g.id.0.clone(),
            store: g.store,
            name: g.name.clone(),
            sort_name: g.sort_name.clone(),
            installed: g.installed,
            size_bytes: g.size_bytes,
            art: ArtFlags { cover: flag(&g.art.cover), hero: flag(&g.art.hero), logo: flag(&g.art.logo), icon: flag(&g.art.icon) },
            art_v,
            playtime_min,
            last_played: g.last_played,
            status: g.status.clone(),
            favourite,
            hidden: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub autostart: bool,
    pub keep_steam_ready: bool,
    /// Keep Proton's Steam Linux Runtime (umu) updated in the background, so games never wait for it.
    pub update_runtime: bool,
    pub show_in_tether: bool,
    pub accent: String,
    pub look: String,

    // ---- Settings page (mockups/08-settings.html). Every field has a default, so older stored settings load.
    /// The login entry starts grapple hidden in the tray (`--hidden`). Only matters with `autostart`.
    pub start_hidden: bool,
    /// The page grapple opens on: `home | library | collection | store`.
    pub open_on: String,
    /// Closing the main window keeps grapple running in the tray (`false`: closing quits).
    pub close_to_tray: bool,
    /// When a game grapple started is running: `card` (shrink to the playing card), `hide`, `keep`.
    pub on_game_start: String,
    /// When that game closes: `show` (bring grapple back) or `stay` (stay hidden).
    pub on_game_exit: String,
    /// Interface size in percent (90, 100, 110, 125).
    pub ui_scale: u32,
    pub reduce_motion: bool,
    /// Stores whose games are left out of every list (`steam`, `epic`).
    pub hidden_stores: Vec<String>,
    /// List games that are not installed (Collection, search).
    pub show_uninstalled: bool,
    /// Games hidden from the library, by id.
    pub hidden_games: Vec<String>,
    /// First day of "this week": `monday` or `sunday`.
    pub week_start: String,
    /// "Close Steam when I quit a game" for games without their own choice (unless Steam was already open).
    pub close_steam: bool,
    /// Linux: the Proton for Epic and other Windows games without their own ("GE-Proton (latest)" or a folder
    /// name). `None`: Heroic's choice.
    pub default_proton: Option<String>,
    /// Linux: GameMode and MangoHud for games without their own choice.
    pub default_gamemode: bool,
    pub default_mangohud: bool,
    /// Friends' games on Home (the "is in …" bar).
    pub friends_on_home: bool,
    /// Hold desktop notifications while a game runs; they come when it closes.
    pub quiet_while_playing: bool,

    // ---- Controllers (Settings › Controllers, src/controllers/).
    /// A connected pad moves the focus in grapple while its window is in front.
    pub pad_nav: bool,
    /// Button prompts: `auto` (by the pad's maker), `playstation`, `xbox`, `nintendo`.
    pub pad_prompts: String,
    /// A short rumble when the pad moves the focus.
    pub pad_vibrate: bool,
    /// Linux: how pads are shown to Epic and other Proton games: `as_is` or `xbox` (grapple_launch::pads).
    pub pad_in_games: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            autostart: false,
            keep_steam_ready: false,
            update_runtime: true,
            show_in_tether: true,
            accent: "green".into(),
            look: "default".into(),
            start_hidden: true,
            open_on: "home".into(),
            close_to_tray: true,
            on_game_start: "card".into(),
            on_game_exit: "show".into(),
            ui_scale: 100,
            reduce_motion: false,
            hidden_stores: Vec::new(),
            show_uninstalled: true,
            hidden_games: Vec::new(),
            week_start: "monday".into(),
            close_steam: true,
            default_proton: None,
            default_gamemode: true,
            default_mangohud: false,
            friends_on_home: true,
            quiet_while_playing: true,
            pad_nav: true,
            pad_prompts: "auto".into(),
            pad_vibrate: false,
            pad_in_games: "as_is".into(),
        }
    }
}

impl Settings {
    /// Is this game left out of the lists (hidden by hand, its store switched off, or not installed while
    /// uninstalled games are off)?
    pub fn hides(&self, id: &str, store: Store, installed: bool) -> bool {
        let store_name = match store {
            Store::Steam => "steam",
            Store::Epic => "epic",
            Store::Manual => "manual",
        };
        self.hidden_games.iter().any(|h| h == id) || self.hidden_stores.iter().any(|s| s == store_name) || (!self.show_uninstalled && !installed)
    }

    /// Do these settings change which games are listed (so the UI needs `games-changed`)?
    pub fn lists_differ(&self, other: &Settings) -> bool {
        self.hidden_games != other.hidden_games || self.hidden_stores != other.hidden_stores || self.show_uninstalled != other.show_uninstalled
    }

    /// Closing the main window hides it (tray) instead of quitting.
    pub fn close_hides(&self) -> bool {
        self.close_to_tray
    }
}

/// Minutes played "this week" from the last 14 local days (oldest first, today last): the days since the week
/// started. `weekday` is today's day counted from Monday (0) to Sunday (6).
pub fn week_minutes(last_14_days: &[u32; 14], weekday: u32, week_start: &str) -> u64 {
    let since_start = if week_start == "sunday" { (weekday + 1) % 7 } else { weekday % 7 } as usize;
    last_14_days[13 - since_start..].iter().map(|m| *m as u64).sum()
}

/// `stats` with "this week" counted from the start of the week in the settings (the Db counts the last 7 days).
pub fn with_week(mut stats: PlayStats, week_start: &str) -> PlayStats {
    use chrono::Datelike;
    let weekday = chrono::Local::now().weekday().num_days_from_monday();
    stats.this_week_min = week_minutes(&stats.last_14_days, weekday, week_start);
    stats
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GameSettings {
    /// `None`: the default in `Settings` (`default_gamemode`, `default_mangohud`, `close_steam`). Older stored
    /// settings have plain booleans here, which load as that game's own choice.
    pub gamemode: Option<bool>,
    pub mangohud: Option<bool>,
    pub close_steam_on_quit: Option<bool>,
    pub show_in_tether: bool,
    /// Epic games on Linux: the Proton picked in the install sheet ("GE-Proton (latest)" or a folder name).
    /// `None`: Heroic's choice.
    pub proton: Option<String>,
    /// Epic games: start with Epic's in-game overlay (where store checkouts such as Rocket League credits show).
    pub eos_overlay: bool,
    /// Linux, Proton games: this game's own `pad_in_games` (`None`: the default in `Settings`).
    pub pad_in_games: Option<String>,
}

impl Default for GameSettings {
    fn default() -> Self {
        GameSettings { gamemode: None, mangohud: None, close_steam_on_quit: None, show_in_tether: true, proton: None, eos_overlay: false, pad_in_games: None }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LibraryStats {
    pub this_week_min: u64,
    pub last_14_days_min: u64,
    pub all_time_min: u64,
    pub most_played: Option<String>,
}

impl LibraryStats {
    /// Sums per-game stats; shared by both backends.
    pub fn sum<'a>(items: impl Iterator<Item = (&'a str, &'a PlayStats)>) -> LibraryStats {
        let mut out = LibraryStats::default();
        let mut best = 0;
        for (id, s) in items {
            out.this_week_min += s.this_week_min;
            out.last_14_days_min += s.last_14_days.iter().map(|m| *m as u64).sum::<u64>();
            out.all_time_min += s.total_min;
            if s.total_min > best {
                best = s.total_min;
                out.most_played = Some(id.to_string());
            }
        }
        out
    }
}

// ---- what the Settings page shows (mockups/08-settings.html); both backends answer with these.

/// Settings › Library: where games come from, the folders grapple reads, the last scan, the hidden games.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LibraryInfo {
    pub sources: Vec<SourceDto>,
    pub folders: Vec<FolderDto>,
    /// Unix seconds of the last finished rescan.
    pub last_scan: Option<i64>,
    pub hidden: Vec<NamedGame>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SourceDto {
    /// `steam | epic | manual`
    pub store: String,
    pub games: usize,
    pub installed: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct FolderDto {
    /// `~/…` under the home folder.
    pub path: String,
    /// `steam | epic`
    pub store: String,
    pub installed: usize,
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NamedGame {
    pub id: String,
    pub name: String,
}

impl SourceDto {
    /// Games and installed games per store, in the page's order (Steam, Epic, added by hand).
    pub fn count(games: &[Game]) -> Vec<SourceDto> {
        [(Store::Steam, "steam"), (Store::Epic, "epic"), (Store::Manual, "manual")]
            .into_iter()
            .map(|(s, name)| SourceDto {
                store: name.into(),
                games: games.iter().filter(|g| g.store == s).count(),
                installed: games.iter().filter(|g| g.store == s && g.installed).count(),
            })
            .collect()
    }
}

/// The hidden games that still exist, with their names, in the order they were hidden.
pub fn hidden_named(settings: &Settings, games: &[Game]) -> Vec<NamedGame> {
    settings
        .hidden_games
        .iter()
        .filter_map(|id| games.iter().find(|g| g.id.0 == *id).map(|g| NamedGame { id: id.clone(), name: g.name.clone() }))
        .collect()
}

/// Settings › Playing (Linux): the Proton versions found and what "Runs with" offers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProtonsDto {
    /// "GE-Proton (latest)" first when there is one, then every version (the names `default_proton` takes).
    pub choices: Vec<String>,
    pub versions: Vec<ProtonDto>,
    /// What Heroic uses when grapple has no choice (a folder name).
    pub heroic_default: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProtonDto {
    pub name: String,
    /// `heroic | steam-tools | steam`
    pub source: String,
}

/// Settings › Accounts: the stores as grapple sees them locally. Never a token.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AccountsDto {
    /// Steam's data folder was found (the library comes from the Steam app).
    pub steam_found: bool,
    /// The Steam profile name (PersonaName) of the most recent login, never the account name.
    pub steam_name: Option<String>,
    /// The store window has site data (cookies): "Sign out of store" can clear it.
    pub store_data: bool,
    /// legendary's login file exists.
    pub epic_signed_in: bool,
    pub epic_name: Option<String>,
}

/// The Files tab of a game's settings.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GameFilesDto {
    pub folder: Option<String>,
    pub size_bytes: Option<u64>,
}

/// Settings › Advanced › Storage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StorageDto {
    pub cache_path: String,
    pub cache_bytes: u64,
    pub data_path: String,
    pub data_bytes: u64,
    pub logs_path: String,
    /// A game log exists ("Copy last launch").
    pub has_log: bool,
}

/// `true` for ids that look like `steam:<appid>`, `epic:<app_name>`, `manual:<uuid>`. Anything else is
/// rejected before it can reach a lookup (the art protocol is reachable from page content).
pub fn valid_game_id(id: &str) -> bool {
    let Some((store, rest)) = id.split_once(':') else { return false };
    matches!(store, "steam" | "epic" | "manual")
        && !rest.is_empty()
        && !rest.starts_with('.')
        && rest.len() <= 128
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// A tether user id as the avatar protocol accepts it: letters, digits and `_` (tether's are decimal). Checked
/// before any lookup, like [`valid_game_id`]. (The same rule as `grapple_tether::valid_user_id`; this copy exists
/// because the `mock` build has no tether crate.)
pub fn valid_user_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A cached profile picture the `grapple://avatar/<user id>` protocol may serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarFile {
    pub path: PathBuf,
    pub mime: &'static str,
    /// The picture's hash: the `?v=` of its URL.
    pub hash: String,
}

/// The art files the scanner returned, by game id, with the art the user picked in their place. The protocol
/// serves only these exact paths; it never builds a path from the request.
#[derive(Default)]
pub struct ArtIndex {
    map: RwLock<HashMap<String, Art>>,
    /// Art the user picked: `<dir>/<game id, ':' as '_'>/<kind>.<png|jpg|webp>` (`$XDG_DATA_HOME/grapple/art`).
    custom: Option<PathBuf>,
}

/// The image types a picked picture may be.
pub const CUSTOM_ART_EXTS: [&str; 3] = ["png", "jpg", "webp"];

impl ArtIndex {
    pub fn with_custom(dir: PathBuf) -> ArtIndex {
        ArtIndex { map: RwLock::default(), custom: Some(dir) }
    }

    /// The folder holding a game's picked art (`None`: no custom art in this build, or a bad id).
    pub fn custom_dir(&self, id: &str) -> Option<PathBuf> {
        valid_game_id(id).then_some(())?;
        Some(self.custom.as_ref()?.join(id.replace(':', "_")))
    }

    /// The picture the user picked for this game and kind, if any.
    pub fn custom_file(&self, id: &str, kind: ArtKind) -> Option<PathBuf> {
        let dir = self.custom_dir(id)?;
        CUSTOM_ART_EXTS.iter().map(|e| dir.join(format!("{}.{e}", kind.name()))).find(|p| p.is_file())
    }

    /// A game's art with the picked pictures in place of the store's.
    pub fn effective(&self, id: &str, art: &Art) -> Art {
        let mut a = art.clone();
        for kind in ArtKind::ALL {
            if let Some(p) = self.custom_file(id, kind) {
                *match kind {
                    ArtKind::Cover => &mut a.cover,
                    ArtKind::Hero => &mut a.hero,
                    ArtKind::Logo => &mut a.logo,
                    ArtKind::Icon => &mut a.icon,
                } = Some(p);
            }
        }
        a
    }

    /// Replace the index with the art of these games; returns each game's `art_v`.
    pub fn update(&self, games: &[Game]) -> HashMap<String, u64> {
        let mut map = HashMap::with_capacity(games.len());
        let mut vs = HashMap::with_capacity(games.len());
        for g in games {
            let art = self.effective(&g.id.0, &g.art);
            let paths = [&art.cover, &art.hero, &art.logo, &art.icon];
            let v = paths
                .iter()
                .filter_map(|p| p.as_deref())
                .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .max()
                .unwrap_or(0);
            vs.insert(g.id.0.clone(), v);
            map.insert(g.id.0.clone(), art);
        }
        *self.map.write().unwrap() = map;
        vs
    }

    pub fn path(&self, id: &str, kind: ArtKind) -> Option<PathBuf> {
        let map = self.map.read().unwrap();
        let art = map.get(id)?;
        match kind {
            ArtKind::Cover => art.cover.clone(),
            ArtKind::Hero => art.hero.clone(),
            ArtKind::Logo => art.logo.clone(),
            ArtKind::Icon => art.icon.clone(),
        }
    }
}

/// Largest picture the user may pick as art.
#[cfg_attr(not(feature = "real"), allow(dead_code))]
const CUSTOM_ART_MAX: u64 = 25 * 1024 * 1024;

/// Copy a picked picture into a game's art folder as `<kind>.<png|jpg|webp>` (the type comes from the file's
/// first bytes, not its name), replacing an earlier pick of that kind.
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub fn save_custom_art(src: &Path, dir: &Path, kind: ArtKind) -> anyhow::Result<()> {
    if std::fs::metadata(src)?.len() > CUSTOM_ART_MAX {
        anyhow::bail!("That picture is over 25 MB");
    }
    let bytes = std::fs::read(src)?;
    let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        anyhow::bail!("Pick a PNG, JPEG or WebP picture");
    };
    std::fs::create_dir_all(dir)?;
    remove_custom_art(dir, kind);
    let tmp = dir.join(format!(".{}.tmp", kind.name()));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dir.join(format!("{}.{ext}", kind.name())))?;
    Ok(())
}

/// Forget the picture picked for one kind of a game's art (the store's own art shows again).
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub fn remove_custom_art(dir: &Path, kind: ArtKind) {
    for e in CUSTOM_ART_EXTS {
        let _ = std::fs::remove_file(dir.join(format!("{}.{e}", kind.name())));
    }
}

/// What the "Add any game" form sends.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ManualDraft {
    pub name: String,
    /// A file path, or a command name for kind `command`.
    pub path: String,
    /// `auto` (decide from the file), `windows`, `appimage`, `native`, `command`.
    pub kind: String,
    /// Extra arguments as typed, split like a shell would.
    pub args: String,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `$XDG_CONFIG_HOME` (`%APPDATA%` on Windows).
pub fn config_dir() -> PathBuf {
    grapple_core::dirs::config_home()
}

/// `$XDG_CACHE_HOME/grapple` (`%LOCALAPPDATA%\grapple\cache` on Windows; tether icons live here).
#[cfg(feature = "real")]
pub fn cache_dir() -> PathBuf {
    grapple_core::dirs::grapple_cache()
}

#[cfg(test)]
mod custom_art_tests {
    use super::*;

    #[test]
    fn picked_art_replaces_the_stores_and_resets() {
        let root = std::env::temp_dir().join(format!("grapple-custom-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let tmp = &root;
        let idx = ArtIndex::with_custom(tmp.join("art"));
        let store = tmp.join("store_cover.jpg");
        std::fs::write(&store, b"store").unwrap();
        let art = Art { cover: Some(store.clone()), ..Art::default() };
        let dir = idx.custom_dir("epic:Sugar").unwrap();
        assert!(idx.custom_dir("epic:../x").is_none(), "bad ids get no folder");

        let png = tmp.join("mine.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();
        save_custom_art(&png, &dir, ArtKind::Cover).unwrap();
        assert_eq!(idx.effective("epic:Sugar", &art).cover, Some(dir.join("cover.png")));
        // A JPEG picked later replaces the PNG; the type comes from the bytes, not the name.
        let jpg = tmp.join("named.png");
        std::fs::write(&jpg, [0xFF, 0xD8, 0xFF, 0xE0, 1, 2]).unwrap();
        save_custom_art(&jpg, &dir, ArtKind::Cover).unwrap();
        assert!(!dir.join("cover.png").exists());
        assert_eq!(idx.custom_file("epic:Sugar", ArtKind::Cover), Some(dir.join("cover.jpg")));

        let text = tmp.join("notes.png");
        std::fs::write(&text, b"not a picture").unwrap();
        assert!(save_custom_art(&text, &dir, ArtKind::Hero).is_err());

        remove_custom_art(&dir, ArtKind::Cover);
        assert_eq!(idx.effective("epic:Sugar", &art).cover, Some(store));
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn old_stored_settings_still_load_with_the_new_defaults() {
        // What grapple stored before the settings page existed.
        let old = r#"{"autostart":true,"keep_steam_ready":false,"update_runtime":true,"show_in_tether":false,"accent":"rose","look":"dim"}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.autostart && !s.show_in_tether);
        assert_eq!((s.accent.as_str(), s.look.as_str()), ("rose", "dim"));
        let d = Settings::default();
        assert_eq!(Settings { autostart: true, show_in_tether: false, accent: "rose".into(), look: "dim".into(), ..d.clone() }, s);
        assert!(d.start_hidden && d.close_to_tray && d.show_uninstalled && d.close_steam && d.default_gamemode && !d.default_mangohud);
        assert_eq!((d.open_on.as_str(), d.on_game_start.as_str(), d.ui_scale, d.week_start.as_str()), ("home", "card", 100, "monday"));
    }

    #[test]
    fn old_game_settings_keep_their_choices_and_new_ones_follow_the_defaults() {
        let old = r#"{"gamemode":false,"mangohud":true,"close_steam_on_quit":true,"show_in_tether":true,"proton":null,"eos_overlay":false}"#;
        let g: GameSettings = serde_json::from_str(old).unwrap();
        assert_eq!((g.gamemode, g.mangohud, g.close_steam_on_quit), (Some(false), Some(true), Some(true)));
        let fresh: GameSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(fresh, GameSettings::default());
        assert_eq!((fresh.gamemode, fresh.mangohud, fresh.close_steam_on_quit), (None, None, None));
    }

    #[test]
    fn hidden_games_stores_and_uninstalled() {
        let mut s = Settings::default();
        assert!(!s.hides("steam:730", Store::Steam, false));
        s.hidden_games = vec!["steam:730".into()];
        assert!(s.hides("steam:730", Store::Steam, true));
        assert!(!s.hides("steam:440", Store::Steam, true));
        s.hidden_stores = vec!["epic".into()];
        assert!(s.hides("epic:Sugar", Store::Epic, true));
        assert!(!s.hides("manual:x", Store::Manual, true));
        s.show_uninstalled = false;
        assert!(s.hides("steam:440", Store::Steam, false) && !s.hides("steam:440", Store::Steam, true));
        let before = Settings::default();
        assert!(s.lists_differ(&before));
        assert!(!before.lists_differ(&Settings { accent: "rose".into(), ..Settings::default() }));
    }

    #[test]
    fn close_to_tray_decides_what_closing_the_window_does() {
        assert!(Settings::default().close_hides());
        assert!(!Settings { close_to_tray: false, ..Settings::default() }.close_hides());
    }

    #[test]
    fn this_week_starts_on_monday_or_sunday() {
        // One hour on each of the last 14 days, today last.
        let days = [60u32; 14];
        // Wednesday (2): Mon, Tue, Wed = 3 days; with Sunday first: Sun..Wed = 4 days.
        assert_eq!(week_minutes(&days, 2, "monday"), 180);
        assert_eq!(week_minutes(&days, 2, "sunday"), 240);
        // Monday (0): only today; Sunday (6) with weeks starting Sunday: only today.
        assert_eq!(week_minutes(&days, 0, "monday"), 60);
        assert_eq!(week_minutes(&days, 6, "sunday"), 60);
        // Sunday with weeks starting Monday: the whole week.
        assert_eq!(week_minutes(&days, 6, "monday"), 420);
        let mut d = [0u32; 14];
        d[12] = 30; // yesterday
        assert_eq!(week_minutes(&d, 0, "monday"), 0);
        assert_eq!(week_minutes(&d, 1, "monday"), 30);
    }
}
