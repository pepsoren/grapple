//! The queue's own SQLite file, `$XDG_DATA_HOME/grapple/downloads.db` (WAL). grapple-core's schema is not touched.
//!
//! `downloads(id TEXT PRIMARY KEY /* GameId */, store, kind TEXT /* install|update */, target_dir, position INTEGER,
//! state TEXT, bytes_done, bytes_total, added_at, finished_at, error)` plus a small `settings` table.
//!
//! `state` holds a [`crate::types::DlState`] word, or `available`: an update grapple knows about that is not queued
//! (shown under "Updates", started by "Update now").

use crate::error::Result;
use crate::types::{DlKind, DlStore};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const AVAILABLE: &str = "available";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub store: DlStore,
    pub kind: DlKind,
    /// The drive folder (Epic: where the game folder goes; Steam: the library).
    pub target_dir: String,
    pub position: i64,
    pub state: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub added_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct QueueDb {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS downloads (
    id TEXT PRIMARY KEY,
    store TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    bytes_total INTEGER,
    added_at INTEGER NOT NULL,
    finished_at INTEGER,
    error TEXT
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn store_of(s: &str) -> DlStore {
    if s == "steam" { DlStore::Steam } else { DlStore::Epic }
}
fn kind_of(s: &str) -> DlKind {
    if s == "update" { DlKind::Update } else { DlKind::Install }
}

impl QueueDb {
    pub fn open(path: &Path) -> Result<QueueDb> {
        if let Some(p) = path.parent()
            && !p.as_os_str().is_empty()
        {
            std::fs::create_dir_all(p)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<QueueDb> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<QueueDb> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(QueueDb { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        Ok(Row {
            id: r.get(0)?,
            store: store_of(&r.get::<_, String>(1)?),
            kind: kind_of(&r.get::<_, String>(2)?),
            target_dir: r.get(3)?,
            position: r.get(4)?,
            state: r.get(5)?,
            bytes_done: r.get::<_, i64>(6)?.max(0) as u64,
            bytes_total: r.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
            added_at: r.get(8)?,
            finished_at: r.get(9)?,
            error: r.get(10)?,
        })
    }

    const COLS: &'static str = "id, store, kind, target_dir, position, state, bytes_done, bytes_total, added_at, finished_at, error";

    pub fn list(&self) -> Result<Vec<Row>> {
        let c = self.lock();
        let mut st = c.prepare(&format!("SELECT {} FROM downloads ORDER BY position, added_at", Self::COLS))?;
        let rows = st.query_map([], Self::row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> Result<Option<Row>> {
        let c = self.lock();
        Ok(c.query_row(&format!("SELECT {} FROM downloads WHERE id = ?1", Self::COLS), [id], Self::row).optional()?)
    }

    pub fn put(&self, r: &Row) -> Result<()> {
        self.lock().execute(
            "INSERT INTO downloads (id, store, kind, target_dir, position, state, bytes_done, bytes_total, added_at, finished_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET store = excluded.store, kind = excluded.kind, target_dir = excluded.target_dir,
               position = excluded.position, state = excluded.state, bytes_done = excluded.bytes_done,
               bytes_total = excluded.bytes_total, added_at = excluded.added_at, finished_at = excluded.finished_at, error = excluded.error",
            params![
                r.id,
                r.store.as_str(),
                r.kind.as_str(),
                r.target_dir,
                r.position,
                r.state,
                r.bytes_done as i64,
                r.bytes_total.map(|v| v as i64),
                r.added_at,
                r.finished_at,
                r.error
            ],
        )?;
        Ok(())
    }

    pub fn set_state(&self, id: &str, state: &str, error: Option<&str>) -> Result<()> {
        self.lock().execute("UPDATE downloads SET state = ?2, error = ?3 WHERE id = ?1", params![id, state, error])?;
        Ok(())
    }

    pub fn set_progress(&self, id: &str, done: u64, total: Option<u64>) -> Result<()> {
        self.lock().execute("UPDATE downloads SET bytes_done = ?2, bytes_total = ?3 WHERE id = ?1", params![id, done as i64, total.map(|t| t as i64)])?;
        Ok(())
    }

    pub fn finish(&self, id: &str, at: i64) -> Result<()> {
        self.lock().execute("UPDATE downloads SET state = 'done', finished_at = ?2, error = NULL WHERE id = ?1", params![id, at])?;
        Ok(())
    }

    pub fn set_position(&self, id: &str, position: i64) -> Result<()> {
        self.lock().execute("UPDATE downloads SET position = ?2 WHERE id = ?1", params![id, position])?;
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.lock().execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn min_position(&self) -> Result<i64> {
        Ok(self.lock().query_row("SELECT COALESCE(MIN(position), 0) FROM downloads WHERE state != 'done'", [], |r| r.get(0))?)
    }

    pub fn max_position(&self) -> Result<i64> {
        Ok(self.lock().query_row("SELECT COALESCE(MAX(position), 0) FROM downloads WHERE state != 'done'", [], |r| r.get(0))?)
    }

    /// Done items: the last 10 within 24 h are kept, older ones dropped.
    pub fn prune_done(&self, now: i64) -> Result<()> {
        let c = self.lock();
        c.execute("DELETE FROM downloads WHERE state = 'done' AND (finished_at IS NULL OR finished_at < ?1)", [now - 86_400])?;
        c.execute(
            "DELETE FROM downloads WHERE state = 'done' AND id NOT IN (SELECT id FROM downloads WHERE state = 'done' ORDER BY finished_at DESC LIMIT 10)",
            [],
        )?;
        Ok(())
    }

    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let v: Option<String> = self.lock().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?;
        Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let s = serde_json::to_string(value).map_err(|e| crate::Error::Invalid(e.to_string()))?;
        self.lock().execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, s])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, pos: i64, state: &str) -> Row {
        Row {
            id: id.into(),
            store: DlStore::Epic,
            kind: DlKind::Install,
            target_dir: "/data/Games".into(),
            position: pos,
            state: state.into(),
            bytes_done: 0,
            bytes_total: Some(10),
            added_at: 100,
            finished_at: None,
            error: None,
        }
    }

    #[test]
    fn rows_round_trip_and_done_is_pruned() {
        let d = tempfile::tempdir().unwrap();
        let db = QueueDb::open(&d.path().join("sub/downloads.db")).unwrap();
        db.put(&row("epic:a", 2, "queued")).unwrap();
        db.put(&row("epic:b", 1, "paused")).unwrap();
        assert_eq!(db.list().unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["epic:b", "epic:a"]);
        db.set_progress("epic:a", 5, Some(10)).unwrap();
        db.set_state("epic:a", "error", Some("Not enough space")).unwrap();
        let a = db.get("epic:a").unwrap().unwrap();
        assert_eq!((a.bytes_done, a.state.as_str(), a.error.as_deref()), (5, "error", Some("Not enough space")));
        for i in 0..12 {
            let mut r = row(&format!("steam:{i}"), 0, "done");
            r.finished_at = Some(1_000_000 + i);
            db.put(&r).unwrap();
        }
        let mut old = row("steam:old", 0, "done");
        old.finished_at = Some(1);
        db.put(&old).unwrap();
        db.prune_done(1_000_100).unwrap();
        let done: Vec<String> = db.list().unwrap().into_iter().filter(|r| r.state == "done").map(|r| r.id).collect();
        assert_eq!(done.len(), 10);
        assert!(!done.contains(&"steam:old".to_string()) && !done.contains(&"steam:0".to_string()));
        db.set_setting("x", &42u32).unwrap();
        assert_eq!(db.setting::<u32>("x").unwrap(), Some(42));
        assert_eq!(db.max_position().unwrap(), 2);
        drop(db);
        // Reopens with the rows.
        assert!(QueueDb::open(&d.path().join("sub/downloads.db")).unwrap().get("epic:b").unwrap().is_some());
    }
}
