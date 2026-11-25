//! "Now playing" for tether, as pure functions: which running game to publish, how its picture is named,
//! how a friend's game maps back to a game in the library, the 128 px PNG icon for non-Steam games, and the
//! stable avatar colour of a friend.

use grapple_core::{Game, GameId, Store, sort_name};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// tether takes icons up to 64 KB.
pub const ICON_MAX_BYTES: usize = 64 * 1024;
/// Square sizes tried in turn until the PNG fits.
const ICON_SIZES: [u32; 3] = [128, 96, 64];

/// How the game's picture reaches tether.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtSource {
    /// `steam:<appid>`: tether fetches Steam's own picture.
    Steam(u32),
    /// Make a PNG from the first of these files that exists and decodes; upload it as `icon:<sha256>`.
    Files(Vec<PathBuf>),
}

/// What to tell tether Josh is playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    pub game: GameId,
    pub name: String,
    /// Unix seconds.
    pub started_at: i64,
    pub art: ArtSource,
}

impl Playing {
    pub fn started_ms(&self) -> i64 {
        self.started_at.saturating_mul(1000)
    }
}

/// The game's picture source: Steam games are named by app id (nothing to upload); Epic and manual games use
/// their icon, else their cover, else their logo.
pub fn art_source(game: &Game) -> ArtSource {
    if game.store == Store::Steam
        && let Ok(appid) = game.id.store_id().parse::<u32>()
    {
        return ArtSource::Steam(appid);
    }
    let files = [&game.art.icon, &game.art.cover, &game.art.logo].into_iter().flatten().cloned().collect();
    ArtSource::Files(files)
}

/// Which running game goes to tether: the newest one started that the settings allow.
///
/// `running` is `(game id, started_at)` as `Launcher::running()` gives it. `exclude` is a game that just exited
/// (its tracking may not be gone yet). `global_show` is "Show what I play in tether"; `game_show` is the
/// per-game switch. Returns `None` when nothing may be shown: tether then has the game cleared.
pub fn choose_playing(
    running: &[(GameId, i64)],
    games: &[Game],
    exclude: Option<&GameId>,
    global_show: bool,
    game_show: impl Fn(&GameId) -> bool,
) -> Option<Playing> {
    if !global_show {
        return None;
    }
    let mut candidates: Vec<&(GameId, i64)> = running.iter().filter(|(id, _)| Some(id) != exclude).collect();
    candidates.sort_by_key(|(id, at)| (std::cmp::Reverse(*at), id.0.clone()));
    for (id, at) in candidates {
        if !game_show(id) {
            continue;
        }
        let Some(game) = games.iter().find(|g| &g.id == id) else { continue };
        return Some(Playing { game: id.clone(), name: game.name.clone(), started_at: *at, art: art_source(game) });
    }
    None
}

// ---------------------------------------------------------------------------------------------- friends

/// What a friend's game is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    pub id: String,
    pub key: String,
}

impl LibraryEntry {
    pub fn of(game: &Game) -> LibraryEntry {
        LibraryEntry { id: game.id.0.clone(), key: sort_name(&game.name) }
    }
}

/// `steam:<digits>` -> the app id (from tether's `large_image`).
pub fn steam_appid(image: &str) -> Option<u32> {
    image.strip_prefix("steam:")?.parse().ok()
}

/// Which library game is this friend's game? By Steam app id when tether names one, else by the name with
/// the same folding the library uses (case, punctuation and "the" ignored). `None` when it is not in the library.
pub fn match_library<'a>(library: &'a [LibraryEntry], name: &str, appid: Option<u32>) -> Option<&'a str> {
    if let Some(app) = appid {
        let want = format!("steam:{app}");
        if let Some(e) = library.iter().find(|e| e.id == want) {
            return Some(&e.id);
        }
    }
    let key = sort_name(name);
    if key.is_empty() {
        return None;
    }
    library.iter().find(|e| e.key == key).map(|e| e.id.as_str())
}

/// The id the UI groups a friend's game under: the library game, else a name key (so friends in the same
/// game you do not own still group together).
pub fn group_id(library: &[LibraryEntry], name: &str, appid: Option<u32>) -> String {
    match match_library(library, name, appid) {
        Some(id) => id.to_string(),
        None => match appid {
            Some(a) => format!("tether:steam:{a}"),
            None => format!("tether:{}", sort_name(name)),
        },
    }
}

/// A stable colour slot for a user (`var(--avatar-<n>)`, 8 of them) from their id.
pub fn avatar_slot(user_id: &str) -> usize {
    // FNV-1a: snowflake ids differ in their low digits, so mixing is needed.
    let mut h: u32 = 0x811c_9dc5;
    for b in user_id.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    (h % 8) as usize
}

// ---------------------------------------------------------------------------------------------- icons

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode `path`, crop the middle square, scale to 128 px and encode a PNG of at most 64 KB (smaller sizes are
/// tried when a busy picture does not fit).
pub fn make_icon_png(path: &Path) -> Result<Vec<u8>, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    icon_from_image(&img)
}

pub fn icon_from_image(img: &image::DynamicImage) -> Result<Vec<u8>, String> {
    use image::imageops::FilterType;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("empty picture".into());
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    for size in ICON_SIZES {
        let small = square.resize_exact(size, size, FilterType::Lanczos3).to_rgba8();
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(small)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        if out.len() <= ICON_MAX_BYTES {
            return Ok(out);
        }
    }
    Err("the picture is too detailed to fit tether's 64 KB icon limit".into())
}

/// What is remembered per game about its uploaded icon (Db setting `tether.icon.<game id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct IconRecord {
    /// SHA-256 of the PNG that was made (the `icon:<sha256>` name).
    pub sha: String,
    /// The picture it was made from, and that file's mtime (unix seconds).
    pub src: String,
    pub mtime: i64,
}

pub fn icon_key(game: &GameId) -> String {
    format!("tether.icon.{}", game.0)
}

/// Is the remembered icon still the one for this source file?
pub fn record_matches(rec: &IconRecord, src: &Path, mtime: i64) -> bool {
    rec.src == src.to_string_lossy() && rec.mtime == mtime && rec.sha.len() == 64 && rec.sha.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapple_core::Art;

    fn game(store: Store, id: &str, name: &str, art: Art) -> Game {
        Game {
            id: GameId::new(store, id),
            store,
            name: name.into(),
            sort_name: sort_name(name),
            installed: true,
            install_dir: None,
            size_bytes: None,
            art,
            store_playtime_min: 0,
            last_played: None,
            status: None,
        }
    }

    fn lib() -> (Vec<Game>, Vec<LibraryEntry>) {
        let games = vec![
            game(Store::Steam, "730", "Counter-Strike 2", Art::default()),
            game(Store::Epic, "Sugar", "Rocket League®", Art { cover: Some("/c.jpg".into()), icon: Some("/i.png".into()), ..Art::default() }),
            game(Store::Manual, "m1", "The Mine", Art::default()),
        ];
        let entries = games.iter().map(LibraryEntry::of).collect();
        (games, entries)
    }

    #[test]
    fn steam_games_are_named_by_app_id_others_by_their_pictures() {
        let (g, _) = lib();
        assert_eq!(art_source(&g[0]), ArtSource::Steam(730));
        assert_eq!(art_source(&g[1]), ArtSource::Files(vec!["/i.png".into(), "/c.jpg".into()]), "icon first, then cover");
        assert_eq!(art_source(&g[2]), ArtSource::Files(vec![]));
    }

    #[test]
    fn chooses_the_newest_running_game_the_settings_allow() {
        let (g, _) = lib();
        let running = vec![(GameId("steam:730".into()), 100), (GameId("epic:Sugar".into()), 200)];
        let all = |_: &GameId| true;
        let p = choose_playing(&running, &g, None, true, all).unwrap();
        assert_eq!((p.game.0.as_str(), p.name.as_str(), p.started_at), ("epic:Sugar", "Rocket League®", 200));
        assert_eq!(p.started_ms(), 200_000);

        // the global switch off: nothing is published
        assert_eq!(choose_playing(&running, &g, None, false, all), None);
        // a per-game switch off skips that game and falls back to the next one
        let p = choose_playing(&running, &g, None, true, |id| id.0 != "epic:Sugar").unwrap();
        assert_eq!(p.game.0, "steam:730");
        assert_eq!(p.art, ArtSource::Steam(730));
        // everything off, or nothing running: cleared
        assert_eq!(choose_playing(&running, &g, None, true, |_| false), None);
        assert_eq!(choose_playing(&[], &g, None, true, all), None);
        // a game that just exited is ignored even if it is still listed
        let p = choose_playing(&running, &g, Some(&GameId("epic:Sugar".into())), true, all).unwrap();
        assert_eq!(p.game.0, "steam:730");
        assert_eq!(choose_playing(&running[1..], &g, Some(&GameId("epic:Sugar".into())), true, all), None);
        // a running game the library does not know is skipped, not published under a blank name
        assert_eq!(choose_playing(&[(GameId("steam:1".into()), 5)], &g, None, true, all), None);
    }

    #[test]
    fn friends_games_map_by_app_id_then_by_name() {
        let (_, l) = lib();
        assert_eq!(match_library(&l, "whatever the friend sees", Some(730)), Some("steam:730"));
        assert_eq!(match_library(&l, "counter strike 2", None), Some("steam:730"), "case and punctuation folded");
        assert_eq!(match_library(&l, "ROCKET LEAGUE", None), Some("epic:Sugar"), "an Epic game matches by name");
        assert_eq!(match_library(&l, "Mine", None), Some("manual:m1"), "leading 'the' ignored");
        assert_eq!(match_library(&l, "Hades II", Some(1145350)), None, "not in the library");
        assert_eq!(match_library(&l, "", None), None);
        // an app id that is not in the library falls back to the name
        assert_eq!(match_library(&l, "Counter-Strike 2", Some(99999)), Some("steam:730"));
    }

    #[test]
    fn unowned_games_still_group_together() {
        let (_, l) = lib();
        assert_eq!(group_id(&l, "Counter-Strike 2", Some(730)), "steam:730");
        assert_eq!(group_id(&l, "Hades II", Some(1145350)), "tether:steam:1145350");
        assert_eq!(group_id(&l, "Some Indie!", None), "tether:some indie");
    }

    #[test]
    fn steam_appid_parses_only_digits() {
        assert_eq!(steam_appid("steam:730"), Some(730));
        assert_eq!(steam_appid("icon:ab"), None);
        assert_eq!(steam_appid("steam:7x"), None);
    }

    #[test]
    fn avatar_slots_are_stable_and_spread() {
        assert_eq!(avatar_slot("123456789"), avatar_slot("123456789"));
        let slots: std::collections::HashSet<_> = (1000..1040).map(|i| avatar_slot(&i.to_string())).collect();
        assert!(slots.len() >= 6, "{slots:?}");
        assert!(slots.iter().all(|s| *s < 8));
    }

    #[test]
    fn icons_are_square_png_within_the_limit() {
        // A 300x200 gradient with noise-free detail: cropped to 200x200 and scaled to 128.
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(300, 200, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 120, 255])));
        let png = icon_from_image(&img).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.len() <= ICON_MAX_BYTES, "{} bytes", png.len());
        let back = image::load_from_memory(&png).unwrap();
        assert_eq!((back.width(), back.height()), (128, 128));
    }

    #[test]
    fn a_noisy_picture_shrinks_until_it_fits() {
        // Incompressible noise at 128 px is over 64 KB of RGBA: it must fall back to a smaller size.
        let mut x: u32 = 12345;
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(128, 128, |_, _| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            image::Rgba([(x >> 24) as u8, (x >> 16) as u8, (x >> 8) as u8, 255])
        }));
        let png = icon_from_image(&img).unwrap();
        assert!(png.len() <= ICON_MAX_BYTES);
        assert!(image::load_from_memory(&png).unwrap().width() < 128);
    }

    #[test]
    fn make_icon_png_reads_files_and_reports_bad_ones() {
        let dir = std::env::temp_dir().join(format!("grapple-icon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("a.png");
        image::RgbaImage::from_pixel(40, 60, image::Rgba([1, 2, 3, 255])).save(&ok).unwrap();
        let png = make_icon_png(&ok).unwrap();
        assert_eq!(sha256_hex(&png).len(), 64);
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not a picture").unwrap();
        assert!(make_icon_png(&bad).is_err());
        assert!(make_icon_png(&dir.join("missing.png")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_cached_icon_record_is_trusted_only_for_the_same_source() {
        let rec = IconRecord { sha: "a".repeat(64), src: "/x/i.png".into(), mtime: 10 };
        assert!(record_matches(&rec, Path::new("/x/i.png"), 10));
        assert!(!record_matches(&rec, Path::new("/x/i.png"), 11), "the picture changed");
        assert!(!record_matches(&rec, Path::new("/x/other.png"), 10));
        assert!(!record_matches(&IconRecord { sha: "zz".into(), ..rec }, Path::new("/x/i.png"), 10));
        assert_eq!(icon_key(&GameId("epic:Sugar".into())), "tether.icon.epic:Sugar");
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
