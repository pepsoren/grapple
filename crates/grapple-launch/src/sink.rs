//! Where play sessions are recorded. `grapple_core::Db` implements it; tests use [`MemorySink`].

use crate::error::Result;
use grapple_core::{Db, GameId, Session, SessionSource};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait SessionSink: Send + Sync + 'static {
    fn start_session(&self, game: &GameId, at: i64, source: SessionSource) -> Result<i64>;
    fn end_session(&self, id: i64, at: i64) -> Result<()>;
    fn open_sessions(&self) -> Result<Vec<Session>>;
    /// Remember "this session was still running at `at`" for each open session (replaces the previous set).
    /// After a crash, the session is closed at this time instead of at "now".
    fn heartbeat(&self, seen: &[(i64, i64)]) -> Result<()>;
    fn last_seen(&self, session: i64) -> Result<Option<i64>>;
}

const LAST_SEEN_KEY: &str = "launch.last_seen";

impl SessionSink for Db {
    fn start_session(&self, game: &GameId, at: i64, source: SessionSource) -> Result<i64> {
        Ok(Db::start_session(self, game, at, source)?)
    }
    fn end_session(&self, id: i64, at: i64) -> Result<()> {
        Ok(Db::end_session(self, id, at)?)
    }
    fn open_sessions(&self) -> Result<Vec<Session>> {
        Ok(Db::open_sessions(self)?)
    }
    fn heartbeat(&self, seen: &[(i64, i64)]) -> Result<()> {
        let map: HashMap<String, i64> = seen.iter().map(|(id, at)| (id.to_string(), *at)).collect();
        Ok(self.set_setting(LAST_SEEN_KEY, &map)?)
    }
    fn last_seen(&self, session: i64) -> Result<Option<i64>> {
        let map: Option<HashMap<String, i64>> = self.setting(LAST_SEEN_KEY)?;
        Ok(map.and_then(|m| m.get(&session.to_string()).copied()))
    }
}

/// In-memory sink for tests.
#[derive(Default)]
pub struct MemorySink {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    next: i64,
    sessions: Vec<Session>,
    seen: HashMap<i64, i64>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn all(&self) -> Vec<Session> {
        self.inner.lock().unwrap().sessions.clone()
    }
    /// Pre-load a session (to test crash recovery).
    pub fn insert_open(&self, game: &GameId, started_at: i64, source: SessionSource, last_seen: Option<i64>) -> i64 {
        let mut g = self.inner.lock().unwrap();
        g.next += 1;
        let id = g.next;
        g.sessions.push(Session { id, game: game.clone(), started_at, ended_at: None, source });
        if let Some(l) = last_seen {
            g.seen.insert(id, l);
        }
        id
    }
}

impl SessionSink for MemorySink {
    fn start_session(&self, game: &GameId, at: i64, source: SessionSource) -> Result<i64> {
        Ok(self.insert_open(game, at, source, None))
    }
    fn end_session(&self, id: i64, at: i64) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        if let Some(s) = g.sessions.iter_mut().find(|s| s.id == id && s.ended_at.is_none()) {
            s.ended_at = Some(at.max(s.started_at));
        }
        Ok(())
    }
    fn open_sessions(&self) -> Result<Vec<Session>> {
        Ok(self.inner.lock().unwrap().sessions.iter().filter(|s| s.ended_at.is_none()).cloned().collect())
    }
    fn heartbeat(&self, seen: &[(i64, i64)]) -> Result<()> {
        self.inner.lock().unwrap().seen = seen.iter().copied().collect();
        Ok(())
    }
    fn last_seen(&self, session: i64) -> Result<Option<i64>> {
        Ok(self.inner.lock().unwrap().seen.get(&session).copied())
    }
}
