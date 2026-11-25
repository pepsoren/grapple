//! The store's own SQLite file (`store.db`, next to Grapple's `grapple.db`): cached lists, details, prices, tags,
//! price history, the wishlist, settings. Same style as `grapple-core`'s `Db`: one connection behind a mutex, WAL,
//! the schema version in `PRAGMA user_version`, `MIGRATIONS` append-only.
//!
//! Lists and feeds are JSON in `kv`; what is looked up by id (details, prices, tags) has its own tables so a
//! front page is a handful of indexed reads.

use crate::error::{Error, Result};
use crate::steam::{AppInfo, PriceLookup, SteamPrice, Tag};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// Each entry upgrades the schema by one version (index + 1). Append only, never edit.
const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, at INTEGER NOT NULL);
     CREATE TABLE info (appid INTEGER NOT NULL, cc TEXT NOT NULL, json TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (appid, cc));
     CREATE TABLE price (
         appid INTEGER NOT NULL, cc TEXT NOT NULL, priced INTEGER NOT NULL,
         currency TEXT NOT NULL DEFAULT '', initial INTEGER NOT NULL DEFAULT 0, final INTEGER NOT NULL DEFAULT 0,
         discount INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL, PRIMARY KEY (appid, cc));
     CREATE TABLE tags (appid INTEGER PRIMARY KEY, json TEXT NOT NULL, at INTEGER NOT NULL);
     CREATE TABLE price_history (
         id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL, store TEXT NOT NULL, currency TEXT NOT NULL,
         initial INTEGER NOT NULL, final INTEGER NOT NULL, discount INTEGER NOT NULL, at INTEGER NOT NULL);
     CREATE INDEX price_history_key ON price_history (key, at);
     CREATE TABLE wish (
         key TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', origin TEXT NOT NULL, hidden INTEGER NOT NULL DEFAULT 0,
         notify INTEGER NOT NULL DEFAULT 1, added_at INTEGER NOT NULL);
     CREATE TABLE epic_offer (key TEXT PRIMARY KEY, json TEXT NOT NULL, at INTEGER NOT NULL);
     CREATE TABLE epic_quote (appid INTEGER NOT NULL, country TEXT NOT NULL, json TEXT, at INTEGER NOT NULL, PRIMARY KEY (appid, country));
     CREATE TABLE itad_gid (appid INTEGER PRIMARY KEY, gid TEXT, at INTEGER NOT NULL);
     CREATE TABLE sale_seen (key TEXT PRIMARY KEY, final INTEGER NOT NULL, at INTEGER NOT NULL);
     CREATE TABLE art_miss (key TEXT NOT NULL, kind TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (key, kind));
     CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    // v2: which Epic offer is a Steam game (a confident match only; a row without a namespace is "no match")
    "CREATE TABLE epic_map (appid INTEGER PRIMARY KEY, namespace TEXT, offer TEXT, slug TEXT, title TEXT, at INTEGER NOT NULL);",
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Clone)]
pub struct StoreDb {
    conn: Arc<Mutex<Connection>>,
}

/// A wishlist row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WishRow {
    pub key: String,
    pub name: String,
    /// `steam` (synced from the public Steam wishlist) or `local` (added in Grapple).
    pub origin: String,
    pub hidden: bool,
    pub notify: bool,
    pub added_at: i64,
}

/// A Steam game matched to an Epic offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicMatch {
    pub namespace: String,
    pub offer: String,
    pub slug: String,
    pub title: String,
}

/// A cached price with the time it was fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPrice {
    pub lookup: PriceLookup,
    pub at: i64,
}

impl StoreDb {
    pub fn open(path: &Path) -> Result<StoreDb> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<StoreDb> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<StoreDb> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(Error::Db(format!("store database version {found} is newer than this build understands ({SCHEMA_VERSION})")));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(StoreDb { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------------------------------------------------------------------------------------------- kv

    pub fn kv_get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<(T, i64)>> {
        let conn = self.lock();
        let row: Option<(String, i64)> = conn.query_row("SELECT value, at FROM kv WHERE key = ?1", params![key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        match row {
            // A value that no longer fits its type (a newer build wrote it) is a miss, not an error.
            Some((v, at)) => Ok(serde_json::from_str(&v).ok().map(|x| (x, at))),
            None => Ok(None),
        }
    }

    pub fn kv_set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.lock().execute(
            "INSERT INTO kv(key, value, at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = ?2, at = ?3",
            params![key, serde_json::to_string(value)?, now()],
        )?;
        Ok(())
    }

    /// Mark a kv entry as stale without deleting it (the old value still renders until the refresh lands).
    pub fn kv_touch(&self, key: &str, at: i64) -> Result<()> {
        self.lock().execute("UPDATE kv SET at = ?2 WHERE key = ?1", params![key, at])?;
        Ok(())
    }

    // ------------------------------------------------------------------------------------------ settings

    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.lock();
        let raw: Option<String> = conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.lock().execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    pub fn clear_setting(&self, key: &str) -> Result<()> {
        self.lock().execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    // --------------------------------------------------------------------------------------------- details

    pub fn put_info(&self, cc: &str, info: &AppInfo) -> Result<()> {
        self.lock().execute(
            "INSERT INTO info(appid, cc, json, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(appid, cc) DO UPDATE SET json = ?3, at = ?4",
            params![info.appid, cc, serde_json::to_string(info)?, now()],
        )?;
        Ok(())
    }

    /// Remember that Steam has nothing for this app (so it is not asked again for a day).
    pub fn put_info_missing(&self, cc: &str, appid: u32) -> Result<()> {
        self.lock().execute(
            "INSERT INTO info(appid, cc, json, at) VALUES (?1, ?2, 'null', ?3) ON CONFLICT(appid, cc) DO UPDATE SET json = 'null', at = ?3",
            params![appid, cc, now()],
        )?;
        Ok(())
    }

    /// (info, fetched at). `info` is `None` for an app Steam has nothing for.
    pub fn infos(&self, cc: &str, ids: &[u32]) -> Result<HashMap<u32, (Option<AppInfo>, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT json, at FROM info WHERE appid = ?1 AND cc = ?2")?;
        let mut out = HashMap::with_capacity(ids.len());
        for &id in ids {
            let row: Option<(String, i64)> = stmt.query_row(params![id, cc], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((json, at)) = row {
                out.insert(id, (serde_json::from_str::<Option<AppInfo>>(&json).ok().flatten(), at));
            }
        }
        Ok(out)
    }

    // --------------------------------------------------------------------------------------------- prices

    /// Store prices from a bulk lookup. Also appends to the price history when the price changed (or the last
    /// row is a day old), so "lowest grapple has seen" has data.
    pub fn put_prices(&self, cc: &str, prices: &HashMap<u32, PriceLookup>) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let at = now();
        for (&id, p) in prices {
            match p {
                PriceLookup::Priced(sp) => {
                    tx.execute(
                        "INSERT INTO price(appid, cc, priced, currency, initial, final, discount, at) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(appid, cc) DO UPDATE SET priced = 1, currency = ?3, initial = ?4, final = ?5, discount = ?6, at = ?7",
                        params![id, cc, sp.currency, sp.initial, sp.r#final, sp.discount, at],
                    )?;
                    record_history(&tx, &format!("steam:{id}"), "steam", &sp.currency, sp.initial, sp.r#final, sp.discount, at)?;
                }
                PriceLookup::NoPrice => {
                    tx.execute(
                        "INSERT INTO price(appid, cc, priced, at) VALUES (?1, ?2, 0, ?3)
                         ON CONFLICT(appid, cc) DO UPDATE SET priced = 0, currency = '', initial = 0, final = 0, discount = 0, at = ?3",
                        params![id, cc, at],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn prices(&self, cc: &str, ids: &[u32]) -> Result<HashMap<u32, CachedPrice>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT priced, currency, initial, final, discount, at FROM price WHERE appid = ?1 AND cc = ?2")?;
        let mut out = HashMap::with_capacity(ids.len());
        for &id in ids {
            let row = stmt
                .query_row(params![id, cc], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, u32>(4)?, r.get::<_, i64>(5)?)))
                .optional()?;
            if let Some((priced, currency, initial, fin, discount, at)) = row {
                let lookup = if priced == 1 { PriceLookup::Priced(SteamPrice { currency, initial, r#final: fin, discount }) } else { PriceLookup::NoPrice };
                out.insert(id, CachedPrice { lookup, at });
            }
        }
        Ok(out)
    }

    /// The last cached price of one app (any age), without touching history. Used to spot new sales.
    /// Make every cached Steam price `secs` older (tests: lets a price expire without waiting).
    #[doc(hidden)]
    pub fn age_prices(&self, secs: i64) -> Result<()> {
        self.lock().execute("UPDATE price SET at = at - ?1", params![secs])?;
        Ok(())
    }

    /// Make every cached Epic price quote `secs` older (tests).
    #[doc(hidden)]
    pub fn age_epic_quotes(&self, secs: i64) -> Result<()> {
        self.lock().execute("UPDATE epic_quote SET at = at - ?1", params![secs])?;
        Ok(())
    }

    pub fn price_one(&self, cc: &str, id: u32) -> Result<Option<CachedPrice>> {
        Ok(self.prices(cc, &[id])?.remove(&id))
    }

    pub fn record_price(&self, key: &str, store: &str, currency: &str, initial: i64, fin: i64, discount: u32) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        record_history(&tx, key, store, currency, initial, fin, discount, now())?;
        tx.commit()?;
        Ok(())
    }

    /// The cheapest recorded price per (store, currency) of a game: `(store, currency, final)`.
    pub fn history_lows(&self, key: &str) -> Result<Vec<(String, String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT store, currency, MIN(final) FROM price_history WHERE key = ?1 GROUP BY store, currency")?;
        let rows = stmt.query_map(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn history_len(&self, key: &str) -> Result<i64> {
        Ok(self.lock().query_row("SELECT COUNT(*) FROM price_history WHERE key = ?1", params![key], |r| r.get(0))?)
    }

    // ------------------------------------------------------------------------------------------------ tags

    pub fn put_tags(&self, appid: u32, tags: &[Tag]) -> Result<()> {
        self.lock().execute(
            "INSERT INTO tags(appid, json, at) VALUES (?1, ?2, ?3) ON CONFLICT(appid) DO UPDATE SET json = ?2, at = ?3",
            params![appid, serde_json::to_string(tags)?, now()],
        )?;
        Ok(())
    }

    pub fn tags(&self, ids: &[u32]) -> Result<HashMap<u32, (Vec<Tag>, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT json, at FROM tags WHERE appid = ?1")?;
        let mut out = HashMap::with_capacity(ids.len());
        for &id in ids {
            let row: Option<(String, i64)> = stmt.query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((json, at)) = row {
                out.insert(id, (serde_json::from_str(&json).unwrap_or_default(), at));
            }
        }
        Ok(out)
    }

    // --------------------------------------------------------------------------------------------- wishlist

    pub fn wish_all(&self) -> Result<Vec<WishRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT key, name, origin, hidden, notify, added_at FROM wish ORDER BY added_at DESC, key")?;
        let rows = stmt.query_map([], |r| Ok(WishRow { key: r.get(0)?, name: r.get(1)?, origin: r.get(2)?, hidden: r.get::<_, i64>(3)? != 0, notify: r.get::<_, i64>(4)? != 0, added_at: r.get(5)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn wish_get(&self, key: &str) -> Result<Option<WishRow>> {
        Ok(self.wish_all()?.into_iter().find(|w| w.key == key))
    }

    /// Add or un-hide a local wishlist entry.
    pub fn wish_add_local(&self, key: &str, name: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO wish(key, name, origin, hidden, notify, added_at) VALUES (?1, ?2, 'local', 0, 1, ?3)
             ON CONFLICT(key) DO UPDATE SET hidden = 0, name = CASE WHEN wish.name = '' THEN ?2 ELSE wish.name END",
            params![key, name, now()],
        )?;
        Ok(())
    }

    /// Remove from the wishlist: local entries are deleted; Steam ones are hidden (grapple never changes the
    /// Steam wishlist itself).
    pub fn wish_remove(&self, key: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM wish WHERE key = ?1 AND origin = 'local'", params![key])?;
        conn.execute("UPDATE wish SET hidden = 1 WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Steam-wishlist entries hidden from grapple's list (newest first).
    pub fn wish_hidden(&self) -> Result<Vec<(String, String)>> {
        Ok(self.wish_all()?.into_iter().filter(|w| w.hidden).map(|w| (w.key, w.name)).collect())
    }

    /// Bring every hidden wishlist entry back; how many there were.
    pub fn wish_unhide_all(&self) -> Result<usize> {
        Ok(self.lock().execute("UPDATE wish SET hidden = 0 WHERE hidden = 1", [])?)
    }

    pub fn wish_set_notify(&self, key: &str, on: bool) -> Result<()> {
        self.lock().execute("UPDATE wish SET notify = ?2 WHERE key = ?1", params![key, on as i64])?;
        Ok(())
    }

    /// Merge the public Steam wishlist: new ids appear, ids no longer there (and not added locally) go.
    /// `date_added` of Steam ones is kept as the order. Returns the keys that are new.
    pub fn wish_sync_steam(&self, items: &[(u32, i64)]) -> Result<Vec<String>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut new = Vec::new();
        let mut seen = Vec::new();
        for &(id, added) in items {
            let key = format!("steam:{id}");
            seen.push(key.clone());
            let n = tx.execute("UPDATE wish SET added_at = ?2 WHERE key = ?1 AND origin = 'steam'", params![key, added])?;
            if n == 0 {
                let had: Option<String> = tx.query_row("SELECT key FROM wish WHERE key = ?1", params![key], |r| r.get(0)).optional()?;
                if had.is_none() {
                    tx.execute("INSERT INTO wish(key, name, origin, hidden, notify, added_at) VALUES (?1, '', 'steam', 0, 1, ?2)", params![key, added])?;
                    new.push(key);
                }
            }
        }
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT key FROM wish WHERE origin = 'steam'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?.into_iter().filter(|k| !seen.contains(k)).collect()
        };
        for k in stale {
            tx.execute("DELETE FROM wish WHERE key = ?1", params![k])?;
        }
        tx.commit()?;
        Ok(new)
    }

    pub fn wish_set_name(&self, key: &str, name: &str) -> Result<()> {
        self.lock().execute("UPDATE wish SET name = ?2 WHERE key = ?1 AND name != ?2", params![key, name])?;
        Ok(())
    }

    // ------------------------------------------------------------------------------------------ epic data

    pub fn put_epic_offer(&self, key: &str, json: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO epic_offer(key, json, at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET json = ?2, at = ?3",
            params![key, json, now()],
        )?;
        Ok(())
    }

    pub fn epic_offer(&self, key: &str) -> Result<Option<(String, i64)>> {
        Ok(self.lock().query_row("SELECT json, at FROM epic_offer WHERE key = ?1", params![key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    pub fn put_epic_quote(&self, appid: u32, country: &str, json: Option<&str>) -> Result<()> {
        self.lock().execute(
            "INSERT INTO epic_quote(appid, country, json, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(appid, country) DO UPDATE SET json = ?3, at = ?4",
            params![appid, country, json, now()],
        )?;
        Ok(())
    }

    /// (quote JSON or None for "not on Epic", fetched at).
    pub fn epic_quotes(&self, country: &str, ids: &[u32]) -> Result<HashMap<u32, (Option<String>, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT json, at FROM epic_quote WHERE appid = ?1 AND country = ?2")?;
        let mut out = HashMap::new();
        for &id in ids {
            let row: Option<(Option<String>, i64)> = stmt.query_row(params![id, country], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some(r) = row {
                out.insert(id, r);
            }
        }
        Ok(out)
    }

    /// A confident Steam -> Epic match (`Some((namespace, offer, slug, title))`), or `None` for "no match".
    pub fn put_epic_map(&self, appid: u32, m: Option<(&str, &str, &str, &str)>) -> Result<()> {
        let (ns, offer, slug, title) = match m {
            Some((a, b, c, d)) => (Some(a), Some(b), Some(c), Some(d)),
            None => (None, None, None, None),
        };
        self.lock().execute(
            "INSERT INTO epic_map(appid, namespace, offer, slug, title, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(appid) DO UPDATE SET namespace = ?2, offer = ?3, slug = ?4, title = ?5, at = ?6",
            params![appid, ns, offer, slug, title, now()],
        )?;
        Ok(())
    }

    pub fn epic_maps(&self, ids: &[u32]) -> Result<HashMap<u32, (Option<EpicMatch>, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT namespace, offer, slug, title, at FROM epic_map WHERE appid = ?1")?;
        let mut out = HashMap::new();
        for &id in ids {
            let row = stmt
                .query_row(params![id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, i64>(4)?)))
                .optional()?;
            if let Some((ns, offer, slug, title, at)) = row {
                let m = match (ns, offer) {
                    (Some(namespace), Some(offer)) => Some(EpicMatch { namespace, offer, slug: slug.unwrap_or_default(), title: title.unwrap_or_default() }),
                    _ => None,
                };
                out.insert(id, (m, at));
            }
        }
        Ok(out)
    }

    /// The store-page slug of a matched offer.
    pub fn epic_map_slug(&self, namespace: &str, offer: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let slug: Option<Option<String>> = conn.query_row("SELECT slug FROM epic_map WHERE namespace = ?1 AND offer = ?2 LIMIT 1", params![namespace, offer], |r| r.get(0)).optional()?;
        Ok(slug.flatten().filter(|s| !s.is_empty()))
    }

    pub fn put_itad_gid(&self, appid: u32, gid: Option<&str>) -> Result<()> {
        self.lock().execute(
            "INSERT INTO itad_gid(appid, gid, at) VALUES (?1, ?2, ?3) ON CONFLICT(appid) DO UPDATE SET gid = ?2, at = ?3",
            params![appid, gid, now()],
        )?;
        Ok(())
    }

    /// `Some(None)`: known not to exist at ITAD.
    pub fn itad_gids(&self, ids: &[u32]) -> Result<HashMap<u32, Option<String>>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT gid FROM itad_gid WHERE appid = ?1")?;
        let mut out = HashMap::new();
        for &id in ids {
            let row: Option<Option<String>> = stmt.query_row(params![id], |r| r.get(0)).optional()?;
            if let Some(g) = row {
                out.insert(id, g);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------------------------------ sale alerts

    pub fn sale_seen(&self, key: &str) -> Result<Option<i64>> {
        Ok(self.lock().query_row("SELECT final FROM sale_seen WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_sale_seen(&self, key: &str, fin: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO sale_seen(key, final, at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET final = ?2, at = ?3",
            params![key, fin, now()],
        )?;
        Ok(())
    }

    pub fn clear_sale_seen(&self, key: &str) -> Result<()> {
        self.lock().execute("DELETE FROM sale_seen WHERE key = ?1", params![key])?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------------- art

    pub fn art_missing(&self, key: &str, kind: &str, max_age: i64) -> Result<bool> {
        let at: Option<i64> = self.lock().query_row("SELECT at FROM art_miss WHERE key = ?1 AND kind = ?2", params![key, kind], |r| r.get(0)).optional()?;
        Ok(at.is_some_and(|t| now() - t < max_age))
    }

    pub fn set_art_missing(&self, key: &str, kind: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO art_miss(key, kind, at) VALUES (?1, ?2, ?3) ON CONFLICT(key, kind) DO UPDATE SET at = ?3",
            params![key, kind, now()],
        )?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn record_history(tx: &rusqlite::Transaction<'_>, key: &str, store: &str, currency: &str, initial: i64, fin: i64, discount: u32, at: i64) -> Result<()> {
    let last: Option<(String, i64, i64)> = tx
        .query_row("SELECT currency, final, at FROM price_history WHERE key = ?1 AND store = ?2 ORDER BY id DESC LIMIT 1", params![key, store], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    // One row per change, plus a heartbeat per day: the history stays small and still shows how long a price held.
    if let Some((c, f, t)) = &last
        && c == currency
        && *f == fin
        && at - *t < 86_400
    {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO price_history(key, store, currency, initial, final, discount, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![key, store, currency, initial, fin, discount, at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(fin: i64, discount: u32) -> PriceLookup {
        PriceLookup::Priced(SteamPrice { currency: "ZAR".into(), initial: 20000, r#final: fin, discount })
    }

    #[test]
    fn opens_migrates_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/store.db");
        let db = StoreDb::open(&p).unwrap();
        db.set_setting("a", &5).unwrap();
        drop(db);
        let db = StoreDb::open(&p).unwrap();
        assert_eq!(db.setting::<i32>("a").unwrap(), Some(5));
        let v: u32 = db.lock().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        Connection::open(&p).unwrap().execute_batch("PRAGMA user_version = 999").unwrap();
        assert!(StoreDb::open(&p).is_err());
    }

    #[test]
    fn prices_round_trip_and_history_dedupes() {
        let db = StoreDb::open_in_memory().unwrap();
        let mut m = HashMap::new();
        m.insert(1, price(10000, 50));
        m.insert(2, PriceLookup::NoPrice);
        db.put_prices("ZA", &m).unwrap();
        db.put_prices("ZA", &m).unwrap(); // same price: no second history row
        let got = db.prices("ZA", &[1, 2, 3]).unwrap();
        assert_eq!(got[&1].lookup, price(10000, 50));
        assert_eq!(got[&2].lookup, PriceLookup::NoPrice);
        assert!(!got.contains_key(&3));
        assert_eq!(db.history_len("steam:1").unwrap(), 1);
        m.insert(1, price(8000, 60));
        db.put_prices("ZA", &m).unwrap();
        assert_eq!(db.history_len("steam:1").unwrap(), 2);
        assert_eq!(db.history_lows("steam:1").unwrap(), vec![("steam".into(), "ZAR".into(), 8000)]);
        assert_eq!(db.history_len("steam:2").unwrap(), 0, "no price, no history");
        // the other country has its own cache
        assert!(db.prices("US", &[1]).unwrap().is_empty());
    }

    #[test]
    fn details_and_missing_apps() {
        let db = StoreDb::open_in_memory().unwrap();
        let info = AppInfo {
            appid: 7,
            name: "X".into(),
            kind: "game".into(),
            is_free: false,
            short: String::new(),
            developers: vec![],
            publishers: vec![],
            release: None,
            coming_soon: false,
            genres: vec!["Action".into()],
            descriptors: vec![2],
            reviews: None,
            shots: vec![],
            price: None,
            windows: true,
            linux: false,
        };
        db.put_info("ZA", &info).unwrap();
        db.put_info_missing("ZA", 8).unwrap();
        let got = db.infos("ZA", &[7, 8, 9]).unwrap();
        assert_eq!(got[&7].0.as_ref().unwrap().genres, ["Action"]);
        assert!(got[&8].0.is_none() && got.contains_key(&8), "a known-missing app is remembered");
        assert!(!got.contains_key(&9));
    }

    #[test]
    fn wishlist_merges_steam_and_local() {
        let db = StoreDb::open_in_memory().unwrap();
        db.wish_add_local("epic:ns:offer", "Bad Cheese").unwrap();
        assert_eq!(db.wish_sync_steam(&[(10, 100), (20, 200)]).unwrap(), ["steam:10", "steam:20"]);
        assert_eq!(db.wish_sync_steam(&[(10, 100), (30, 300)]).unwrap(), ["steam:30"], "20 left the Steam wishlist");
        let keys: Vec<String> = db.wish_all().unwrap().into_iter().map(|w| w.key).collect();
        assert!(keys.contains(&"steam:10".into()) && keys.contains(&"steam:30".into()) && keys.contains(&"epic:ns:offer".into()));
        assert!(!keys.contains(&"steam:20".into()));
        // removing a Steam entry hides it, and a later sync does not bring it back
        db.wish_remove("steam:10").unwrap();
        db.wish_sync_steam(&[(10, 100), (30, 300)]).unwrap();
        assert!(db.wish_get("steam:10").unwrap().unwrap().hidden);
        // adding it again shows it
        assert_eq!(db.wish_hidden().unwrap().len(), 1);
        assert_eq!(db.wish_unhide_all().unwrap(), 1);
        assert!(!db.wish_get("steam:10").unwrap().unwrap().hidden && db.wish_hidden().unwrap().is_empty());
        db.wish_remove("steam:10").unwrap();
        db.wish_add_local("steam:10", "").unwrap();
        assert!(!db.wish_get("steam:10").unwrap().unwrap().hidden);
        // removing a local entry deletes it
        db.wish_remove("epic:ns:offer").unwrap();
        assert!(db.wish_get("epic:ns:offer").unwrap().is_none());
        db.wish_set_notify("steam:30", false).unwrap();
        assert!(!db.wish_get("steam:30").unwrap().unwrap().notify);
    }

    #[test]
    fn epic_matches_are_kept_with_their_misses() {
        let db = StoreDb::open_in_memory().unwrap();
        db.put_epic_map(1, Some(("ns", "offer", "core-keeper", "Core Keeper"))).unwrap();
        db.put_epic_map(2, None).unwrap();
        let m = db.epic_maps(&[1, 2, 3]).unwrap();
        assert_eq!(m[&1].0, Some(EpicMatch { namespace: "ns".into(), offer: "offer".into(), slug: "core-keeper".into(), title: "Core Keeper".into() }));
        assert!(m[&2].0.is_none() && m.contains_key(&2), "a miss is remembered");
        assert!(!m.contains_key(&3));
        assert_eq!(db.epic_map_slug("ns", "offer").unwrap().as_deref(), Some("core-keeper"));
        assert_eq!(db.epic_map_slug("ns", "other").unwrap(), None);
    }

    #[test]
    fn kv_and_art_misses() {
        let db = StoreDb::open_in_memory().unwrap();
        db.kv_set("list", &vec![1, 2, 3]).unwrap();
        let (v, at): (Vec<i32>, i64) = db.kv_get("list").unwrap().unwrap();
        assert_eq!(v, [1, 2, 3]);
        assert!(at > 0);
        assert!(db.kv_get::<Vec<String>>("list").unwrap().is_none(), "a value of another shape is a miss");
        assert!(!db.art_missing("steam:1", "cover", 3600).unwrap());
        db.set_art_missing("steam:1", "cover").unwrap();
        assert!(db.art_missing("steam:1", "cover", 3600).unwrap());
        assert!(!db.art_missing("steam:1", "hero", 3600).unwrap());
    }
}
