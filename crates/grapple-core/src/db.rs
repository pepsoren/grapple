//! The SQLite store: scan cache, play sessions, favourites, settings.
//!
//! One connection behind a mutex (`Db` is cheap to clone). WAL mode; the schema version lives in
//! `PRAGMA user_version` and `MIGRATIONS` brings older files up to date on open.

use crate::model::{Game, GameId, PlayStats, Session, SessionSource};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database schema version {found} is newer than this build understands ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("no session with id {0}")]
    NoSuchSession(i64),
    #[error("could not work out the data directory (no home directory?)")]
    NoDataDir,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Each entry upgrades the schema by one version (index + 1). Append only, never edit.
const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE games (
         id        TEXT PRIMARY KEY,
         store     TEXT NOT NULL,
         sort_name TEXT NOT NULL,
         json      TEXT NOT NULL
     );
     CREATE INDEX games_sort ON games(sort_name);
     CREATE TABLE sessions (
         id         INTEGER PRIMARY KEY AUTOINCREMENT,
         game       TEXT NOT NULL,
         started_at INTEGER NOT NULL,
         ended_at   INTEGER,
         source     TEXT NOT NULL
     );
     CREATE INDEX sessions_game ON sessions(game, started_at);
     CREATE INDEX sessions_open ON sessions(ended_at) WHERE ended_at IS NULL;
     CREATE TABLE favourites (
         game     TEXT PRIMARY KEY,
         added_at INTEGER NOT NULL
     );
     CREATE TABLE settings (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     );",
];

/// Highest schema version this build knows.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// `$XDG_DATA_HOME/grapple/grapple.db` (usually `~/.local/share/grapple/grapple.db`);
/// `%LOCALAPPDATA%\grapple\grapple.db` on Windows.
#[cfg(not(windows))]
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "grapple").ok_or(Error::NoDataDir)?;
    Ok(dirs.data_dir().join("grapple.db"))
}

/// `$XDG_DATA_HOME/grapple/grapple.db` (usually `~/.local/share/grapple/grapple.db`);
/// `%LOCALAPPDATA%\grapple\grapple.db` on Windows.
#[cfg(windows)]
pub fn default_db_path() -> Result<PathBuf> {
    Ok(crate::dirs::grapple_data().join("grapple.db"))
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_default() -> Result<Db> {
        Self::open(&default_db_path()?)
    }

    /// An in-memory database (tests, previews).
    pub fn open_in_memory() -> Result<Db> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Db> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // journal_mode returns a row, so it has to be read rather than executed.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        migrate(&mut conn)?;
        Ok(Db { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave the connection half-updated (every write is a
        // transaction or a single statement), so a poisoned mutex is safe to keep using.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Store a scan result. Games missing from `games` are removed from the cache, except manual games
    /// (user data: removed only by [`Db::remove_game`]). Favourites and sessions are never touched.
    pub fn replace_games(&self, games: &[Game]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS scan_seen (id TEXT PRIMARY KEY); DELETE FROM scan_seen;")?;
            let mut seen = tx.prepare_cached("INSERT OR REPLACE INTO scan_seen(id) VALUES (?1)")?;
            let mut up = tx.prepare_cached(
                "INSERT INTO games(id, store, sort_name, json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET store = ?2, sort_name = ?3, json = ?4",
            )?;
            for g in games {
                let store = serde_json::to_value(g.store)?;
                up.execute(params![g.id.0, store.as_str().unwrap_or(""), g.sort_name, serde_json::to_string(g)?])?;
                seen.execute(params![g.id.0])?;
            }
        }
        tx.execute("DELETE FROM games WHERE store != 'manual' AND id NOT IN (SELECT id FROM scan_seen)", [])?;
        tx.execute("DELETE FROM scan_seen", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Remove one game from the cache (used for manual entries). Sessions and favourites stay.
    pub fn remove_game(&self, game: &GameId) -> Result<()> {
        self.lock().execute("DELETE FROM games WHERE id = ?1", params![game.0])?;
        Ok(())
    }

    /// All cached games, ordered by `sort_name`.
    pub fn games(&self) -> Result<Vec<Game>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT json FROM games ORDER BY sort_name, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }

    /// Record that a session started; returns its id.
    pub fn start_session(&self, game: &GameId, at: i64, source: SessionSource) -> Result<i64> {
        let source = serde_json::to_value(source)?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO sessions(game, started_at, ended_at, source) VALUES (?1, ?2, NULL, ?3)",
            params![game.0, at, source.as_str().unwrap_or("grapple")],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Close a session. An end before the start is clamped to the start; closing twice keeps the first end.
    pub fn end_session(&self, id: i64, at: i64) -> Result<()> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE sessions SET ended_at = MAX(?2, started_at) WHERE id = ?1 AND ended_at IS NULL",
            params![id, at],
        )?;
        if n == 0 {
            let exists: Option<i64> =
                conn.query_row("SELECT id FROM sessions WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
            if exists.is_none() {
                return Err(Error::NoSuchSession(id));
            }
        }
        Ok(())
    }

    /// Sessions that have no end yet (the game is running, or Grapple died while it ran).
    pub fn open_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, game, started_at, ended_at, source FROM sessions WHERE ended_at IS NULL ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Totals for one game.
    ///
    /// * `total_min` is `max(store_playtime_min, our session minutes)`, never their sum: Steam already counts
    ///   sessions that Grapple started.
    /// * Open sessions count up to `now`.
    /// * `this_week_min` is the last seven local days including today (the last seven bars of the chart).
    /// * `last_14_days` is oldest first; a session that crosses local midnight is split between the days.
    pub fn stats(&self, game: &GameId, store_playtime_min: u64, now: i64) -> Result<PlayStats> {
        self.stats_in(game, store_playtime_min, now, &Local)
    }

    /// [`Db::stats`] with an explicit time zone (the tests use fixed offsets).
    pub fn stats_in<Tz: TimeZone>(&self, game: &GameId, store_playtime_min: u64, now: i64, tz: &Tz) -> Result<PlayStats> {
        let spans: Vec<(i64, i64)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare_cached("SELECT started_at, ended_at FROM sessions WHERE game = ?1")?;
            let rows = stmt.query_map(params![game.0], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)))?;
            let mut v = Vec::new();
            for r in rows {
                let (start, end) = r?;
                v.push((start, end.unwrap_or(now).max(start)));
            }
            v
        };

        let mut our_secs: u64 = 0;
        let mut longest_secs: u64 = 0;
        for &(s, e) in &spans {
            let d = (e - s) as u64;
            our_secs += d;
            longest_secs = longest_secs.max(d);
        }

        let bounds = day_bounds(tz, now);
        let mut day_secs = [0u64; 14];
        for &(s, e) in &spans {
            for (i, secs) in day_secs.iter_mut().enumerate() {
                let lo = s.max(bounds[i]);
                let hi = e.min(bounds[i + 1]);
                if hi > lo {
                    *secs += (hi - lo) as u64;
                }
            }
        }
        let mut last_14_days = [0u32; 14];
        for (slot, secs) in last_14_days.iter_mut().zip(day_secs) {
            *slot = u32::try_from(secs / 60).unwrap_or(u32::MAX);
        }
        let this_week_min = day_secs[7..].iter().sum::<u64>() / 60;

        Ok(PlayStats {
            total_min: store_playtime_min.max(our_secs / 60),
            sessions: spans.len() as u64,
            longest_min: longest_secs / 60,
            this_week_min,
            last_14_days,
        })
    }

    /// When each game was last played by our sessions: the newest session end (or start, while it runs), Unix seconds.
    pub fn last_played_by_sessions(&self) -> Result<std::collections::HashMap<String, i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT game, MAX(COALESCE(ended_at, started_at)) FROM sessions GROUP BY game")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn favourites(&self) -> Result<Vec<GameId>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT game FROM favourites ORDER BY added_at, game")?;
        let rows = stmt.query_map([], |r| Ok(GameId(r.get(0)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_favourite(&self, game: &GameId, on: bool) -> Result<()> {
        let conn = self.lock();
        if on {
            let now = chrono::Utc::now().timestamp();
            conn.execute("INSERT OR IGNORE INTO favourites(game, added_at) VALUES (?1, ?2)", params![game.0, now])?;
        } else {
            conn.execute("DELETE FROM favourites WHERE game = ?1", params![game.0])?;
        }
        Ok(())
    }

    /// A setting stored as JSON. `None` if it was never set; a stored value that does not fit `T` is an error.
    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.lock();
        let raw: Option<String> =
            conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()?;
        match raw {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// A setting's stored text exactly as written, valid JSON or not. `None` if it was never set.
    pub fn setting_raw(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    /// Store text as-is (no JSON encoding). Only for tests and repairs that need a value `set_setting` cannot make.
    pub fn set_setting_raw(&self, key: &str, text: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, text],
        )?;
        Ok(())
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
        self.lock().execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, json],
        )?;
        Ok(())
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(Error::SchemaTooNew { found, supported: SCHEMA_VERSION });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // PRAGMA does not take bound parameters; the number comes from our own table.
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let source: String = r.get(4)?;
    Ok(Session {
        id: r.get(0)?,
        game: GameId(r.get(1)?),
        started_at: r.get(2)?,
        ended_at: r.get(3)?,
        source: if source == "detected" { SessionSource::Detected } else { SessionSource::Grapple },
    })
}

/// 15 unix timestamps: local midnight of each of the last 14 days (oldest first) plus the end of today.
/// Day `i` is `[bounds[i], bounds[i + 1])`. Built from calendar dates, so DST days come out 23/25 h long.
fn day_bounds<Tz: TimeZone>(tz: &Tz, now: i64) -> [i64; 15] {
    let today = tz
        .timestamp_opt(now, 0)
        .single()
        .map(|t| t.date_naive())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    let mut out = [0i64; 15];
    for (i, slot) in out.iter_mut().enumerate() {
        let date = today + Duration::days(i as i64 - 13);
        *slot = local_midnight(tz, date);
    }
    out
}

fn local_midnight<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> i64 {
    let midnight: NaiveDateTime = date.and_hms_opt(0, 0, 0).expect("valid time");
    match tz.from_local_datetime(&midnight) {
        chrono::LocalResult::Single(t) => t.timestamp(),
        chrono::LocalResult::Ambiguous(first, _) => first.timestamp(),
        // Midnight skipped by a DST jump: use the first instant that exists after it.
        chrono::LocalResult::None => {
            let shifted = midnight + Duration::hours(1);
            tz.from_local_datetime(&shifted).earliest().map(|t| t.timestamp()).unwrap_or_else(|| midnight.and_utc().timestamp())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Art, Store};
    use chrono::FixedOffset;

    fn game(store: Store, id: &str, name: &str) -> Game {
        Game {
            id: GameId::new(store, id),
            store,
            name: name.to_string(),
            sort_name: crate::sort_name(name),
            installed: true,
            install_dir: Some(PathBuf::from("/games").join(id)),
            size_bytes: Some(42),
            art: Art { cover: Some(PathBuf::from("/art/c.jpg")), ..Art::default() },
            store_playtime_min: 10,
            last_played: Some(1_700_000_000),
            status: None,
        }
    }

    fn id(s: &str) -> GameId {
        GameId(s.to_string())
    }

    const H: i64 = 3600;
    const DAY: i64 = 86_400;
    /// 2026-06-15 12:00:00 UTC
    const NOON: i64 = 1_781_524_800;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    #[test]
    fn open_creates_dirs_wal_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/grapple.db");
        let db = Db::open(&path).unwrap();
        assert!(path.exists());
        let mode: String = db.lock().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        let v: u32 = db.lock().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn reopen_keeps_data_and_does_not_remigrate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        {
            let db = Db::open(&path).unwrap();
            db.replace_games(&[game(Store::Steam, "1", "One")]).unwrap();
            db.set_setting("k", &5u32).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.games().unwrap().len(), 1);
        assert_eq!(db.setting::<u32>("k").unwrap(), Some(5));
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        Db::open(&path).unwrap();
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 999").unwrap();
        assert!(matches!(Db::open(&path), Err(Error::SchemaTooNew { found: 999, .. })));
    }

    #[test]
    fn migrates_from_empty_v0_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        // A file that exists but was never initialised.
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE unrelated(x)").unwrap();
        let db = Db::open(&path).unwrap();
        db.set_favourite(&id("steam:1"), true).unwrap();
        assert_eq!(db.favourites().unwrap(), vec![id("steam:1")]);
    }

    #[test]
    fn replace_games_round_trips_and_sorts() {
        let db = Db::open_in_memory().unwrap();
        let b = game(Store::Epic, "b", "The Beta");
        let a = game(Store::Steam, "9", "Alpha");
        db.replace_games(&[b.clone(), a.clone()]).unwrap();
        assert_eq!(db.games().unwrap(), vec![a, b]);
    }

    #[test]
    fn replace_games_drops_stale_but_keeps_favourites_sessions_and_manual() {
        let db = Db::open_in_memory().unwrap();
        let a = game(Store::Steam, "1", "A");
        let b = game(Store::Steam, "2", "B");
        let m = game(Store::Manual, "uuid-1", "Mine");
        db.replace_games(&[a.clone(), b.clone(), m.clone()]).unwrap();
        db.set_favourite(&b.id, true).unwrap();
        let s = db.start_session(&b.id, 1000, SessionSource::Grapple).unwrap();
        db.end_session(s, 2000).unwrap();

        let mut a2 = a.clone();
        a2.name = "A renamed".into();
        db.replace_games(&[a2.clone()]).unwrap();

        let ids: Vec<_> = db.games().unwrap().into_iter().map(|g| g.id.0).collect();
        assert_eq!(ids, vec!["steam:1".to_string(), "manual:uuid-1".to_string()]);
        assert_eq!(db.games().unwrap()[0].name, "A renamed");
        assert_eq!(db.favourites().unwrap(), vec![b.id.clone()]);
        assert_eq!(db.stats_in(&b.id, 0, 3000, &utc()).unwrap().sessions, 1);

        db.remove_game(&m.id).unwrap();
        assert_eq!(db.games().unwrap().len(), 1);
        // Scan again with an empty list: nothing left to keep.
        db.replace_games(&[]).unwrap();
        assert!(db.games().unwrap().is_empty());
    }

    #[test]
    fn last_played_by_sessions_uses_the_newest_end_or_the_start_of_a_running_one() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (id("steam:1"), id("manual:x"));
        let s = db.start_session(&a, 100, SessionSource::Grapple).unwrap();
        db.end_session(s, 200).unwrap();
        let s = db.start_session(&a, 300, SessionSource::Grapple).unwrap();
        db.end_session(s, 400).unwrap();
        db.start_session(&b, 500, SessionSource::Detected).unwrap();
        let m = db.last_played_by_sessions().unwrap();
        assert_eq!((m.get("steam:1"), m.get("manual:x"), m.get("steam:2")), (Some(&400), Some(&500), None));
    }

    #[test]
    fn session_lifecycle() {
        let db = Db::open_in_memory().unwrap();
        let g = id("steam:7");
        let s1 = db.start_session(&g, 100, SessionSource::Grapple).unwrap();
        let s2 = db.start_session(&id("epic:x"), 150, SessionSource::Detected).unwrap();
        let open = db.open_sessions().unwrap();
        assert_eq!(open.len(), 2);
        assert_eq!(open[0], Session { id: s1, game: g.clone(), started_at: 100, ended_at: None, source: SessionSource::Grapple });
        assert_eq!(open[1].source, SessionSource::Detected);

        db.end_session(s1, 400).unwrap();
        db.end_session(s1, 9999).unwrap(); // second close is a no-op
        let open = db.open_sessions().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, s2);

        // Clamped: ending before the start gives a zero-length session, not a negative one.
        db.end_session(s2, 10).unwrap();
        assert!(db.open_sessions().unwrap().is_empty());
        let st = db.stats_in(&id("epic:x"), 0, 500, &utc()).unwrap();
        assert_eq!((st.sessions, st.total_min, st.longest_min), (1, 0, 0));

        assert!(matches!(db.end_session(12345, 1), Err(Error::NoSuchSession(12345))));
        let st = db.stats_in(&g, 0, 500, &utc()).unwrap();
        assert_eq!(st.sessions, 1);
    }

    #[test]
    fn favourites_toggle() {
        let db = Db::open_in_memory().unwrap();
        db.set_favourite(&id("steam:1"), true).unwrap();
        db.set_favourite(&id("steam:1"), true).unwrap();
        db.set_favourite(&id("epic:2"), true).unwrap();
        assert_eq!(db.favourites().unwrap().len(), 2);
        db.set_favourite(&id("steam:1"), false).unwrap();
        assert_eq!(db.favourites().unwrap(), vec![id("epic:2")]);
        db.set_favourite(&id("steam:1"), false).unwrap(); // removing a missing one is fine
    }

    #[test]
    fn settings_round_trip() {
        #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Cfg {
            keep_steam_ready: bool,
            accents: Vec<String>,
        }
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.setting::<Cfg>("cfg").unwrap(), None);
        let c = Cfg { keep_steam_ready: true, accents: vec!["violet".into()] };
        db.set_setting("cfg", &c).unwrap();
        assert_eq!(db.setting::<Cfg>("cfg").unwrap(), Some(c));
        db.set_setting("cfg", &Cfg { keep_steam_ready: false, accents: vec![] }).unwrap();
        assert!(!db.setting::<Cfg>("cfg").unwrap().unwrap().keep_steam_ready);
        db.set_setting("n", &"text").unwrap();
        assert_eq!(db.setting::<String>("n").unwrap().as_deref(), Some("text"));
    }

    #[test]
    fn stats_total_is_max_not_sum() {
        let db = Db::open_in_memory().unwrap();
        let g = id("steam:1");
        let s = db.start_session(&g, NOON - 2 * H, SessionSource::Grapple).unwrap();
        db.end_session(s, NOON - H).unwrap(); // 60 min of ours
        assert_eq!(db.stats_in(&g, 500, NOON, &utc()).unwrap().total_min, 500);
        assert_eq!(db.stats_in(&g, 20, NOON, &utc()).unwrap().total_min, 60);
        assert_eq!(db.stats_in(&g, 0, NOON, &utc()).unwrap().total_min, 60);
    }

    #[test]
    fn stats_counts_longest_sessions_and_open_session() {
        let db = Db::open_in_memory().unwrap();
        let g = id("steam:1");
        let a = db.start_session(&g, NOON - 5 * DAY, SessionSource::Grapple).unwrap();
        db.end_session(a, NOON - 5 * DAY + 90 * 60).unwrap(); // 90 min
        let b = db.start_session(&g, NOON - 2 * DAY, SessionSource::Detected).unwrap();
        db.end_session(b, NOON - 2 * DAY + 30 * 60).unwrap(); // 30 min
        db.start_session(&g, NOON - 45 * 60, SessionSource::Grapple).unwrap(); // still running: 45 min
        let st = db.stats_in(&g, 0, NOON, &utc()).unwrap();
        assert_eq!(st.sessions, 3);
        assert_eq!(st.longest_min, 90);
        assert_eq!(st.total_min, 165);
        assert_eq!(st.this_week_min, 165);
        // Other games are not mixed in.
        assert_eq!(db.stats_in(&id("steam:2"), 0, NOON, &utc()).unwrap(), PlayStats::default());
    }

    #[test]
    fn stats_last_14_days_oldest_first() {
        let db = Db::open_in_memory().unwrap();
        let g = id("steam:1");
        // today (index 13): 20 min; yesterday (12): 10 min; 13 days ago (0): 5 min; 14 days ago: outside.
        for (start, mins) in [(NOON, 20), (NOON - DAY, 10), (NOON - 13 * DAY, 5), (NOON - 14 * DAY, 99)] {
            let s = db.start_session(&g, start, SessionSource::Grapple).unwrap();
            db.end_session(s, start + mins * 60).unwrap();
        }
        let st = db.stats_in(&g, 0, NOON + H, &utc()).unwrap();
        let mut want = [0u32; 14];
        want[13] = 20;
        want[12] = 10;
        want[0] = 5;
        assert_eq!(st.last_14_days, want);
        assert_eq!(st.this_week_min, 30);
        assert_eq!(st.total_min, 134);
    }

    #[test]
    fn stats_splits_sessions_at_local_midnight() {
        let db = Db::open_in_memory().unwrap();
        let g = id("steam:1");
        // 23:00 yesterday to 02:00 today, UTC: 60 min yesterday, 120 min today.
        let midnight = NOON - 12 * H;
        let s = db.start_session(&g, midnight - H, SessionSource::Grapple).unwrap();
        db.end_session(s, midnight + 2 * H).unwrap();
        let st = db.stats_in(&g, 0, NOON, &utc()).unwrap();
        assert_eq!((st.last_14_days[12], st.last_14_days[13]), (60, 120));
        assert_eq!(st.longest_min, 180);

        // The same instants in UTC+2 (midnight is 22:00 UTC): 00:00..02:00 UTC is all "today" (01:00-04:00),
        // and the previous local midnight is 22:00 UTC on the day before, so the split moves.
        let plus2 = FixedOffset::east_opt(2 * 3600).unwrap();
        let st = db.stats_in(&g, 0, NOON, &plus2).unwrap();
        // Local midnight is 22:00 UTC; the session runs 23:00-02:00 UTC, which is entirely after it.
        assert_eq!((st.last_14_days[12], st.last_14_days[13]), (0, 180));
        // And a session spanning many days is split across all of them.
        let s = db.start_session(&id("steam:2"), NOON - 3 * DAY, SessionSource::Grapple).unwrap();
        db.end_session(s, NOON).unwrap();
        let st = db.stats_in(&id("steam:2"), 0, NOON, &utc()).unwrap();
        assert_eq!(st.last_14_days[10..], [12 * 60, 24 * 60, 24 * 60, 12 * 60]);
    }

    #[test]
    fn day_bounds_handle_dst_lengths() {
        // Any tz with DST would do; the sum of day lengths must still tile the window without gaps.
        let b = day_bounds(&utc(), NOON);
        for w in b.windows(2) {
            assert_eq!(w[1] - w[0], DAY);
        }
        assert!(b[13] <= NOON && NOON < b[14]);
    }

    #[test]
    fn clones_share_one_database() {
        let db = Db::open_in_memory().unwrap();
        let db2 = db.clone();
        db.set_setting("x", &1).unwrap();
        assert_eq!(db2.setting::<i32>("x").unwrap(), Some(1));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let d = db.clone();
                std::thread::spawn(move || {
                    for n in 0..20 {
                        d.start_session(&id(&format!("steam:{i}")), n, SessionSource::Grapple).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(db.open_sessions().unwrap().len(), 80);
    }
}
