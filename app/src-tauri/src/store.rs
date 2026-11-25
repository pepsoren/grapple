//! The store's glue to the app: the library the store needs (owned games, friends, Steam's own files), the
//! events the UI listens to (`store-changed`, `store-sale`), the desktop notification for a sale, and the thin
//! functions the Tauri commands in `commands.rs` call. The data side is the `grapple-store` crate.
//!
//! Without the `real` feature (the UI-only mock build) every store command answers "not available": the mock
//! build never reaches a store.

/// What `grapple://store/<key>/<kind>` answers with.
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub enum Served {
    File { path: std::path::PathBuf, mime: &'static str, mtime: u64, len: u64 },
    /// The store has no such picture.
    Missing,
    /// Not a valid key or kind.
    Bad,
    /// The store is not running, or the network failed (try again later).
    Unavailable,
}

#[cfg(feature = "real")]
pub use real::*;
#[cfg(not(feature = "real"))]
pub use stub::*;

#[cfg(not(feature = "real"))]
mod stub {
    use serde_json::Value;
    use tauri::AppHandle;

    type Res = Result<Value, String>;
    const NO: &str = "The store is not available in this build";

    pub fn init(_app: &AppHandle) {}
    pub async fn call(_app: &AppHandle, _what: &str, _args: Value) -> Res {
        Err(NO.into())
    }
    pub async fn art<R: tauri::Runtime>(_app: &tauri::AppHandle<R>, _key: &str, _kind: &str) -> super::Served {
        super::Served::Unavailable
    }
}

#[cfg(feature = "real")]
mod real {
    use crate::backend::cache_dir;
    use grapple_core::{Db, Game, Store};
    use grapple_store::epic_account::{EpicSession, EpicSessionProvider};
    use grapple_store::secrets::{KeyStore, KeyringKeys, MemoryKeys};
    use grapple_store::{Friend, Library, OwnedGame, SaleAlert, StoreConfig, StoreEvent, StoreFilters, StoreKind, StoreService, StoreSettings};
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use tauri::{AppHandle, Emitter, Manager};

    type Res = Result<Value, String>;

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    // ------------------------------------------------------------------------------------------ library

    /// How long the owned-games list is reused: a front build asks for it once, and a burst of store calls
    /// should not rescan the folders.
    const OWNED_TTL: Duration = Duration::from_secs(5);

    pub struct AppLibrary {
        app: AppHandle,
        db: Db,
        steam_root: Option<PathBuf>,
        legendary_dir: Option<PathBuf>,
        owned: Mutex<Option<(Instant, Vec<OwnedGame>)>>,
    }

    fn secs(t: SystemTime) -> Option<i64> {
        t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
    }

    /// `grapple://art/<id>/<kind>?v=<mtime>` for a file that exists (Windows: the localhost form).
    fn art_url(id: &str, kind: &str, file: &Path) -> Option<String> {
        let v = secs(std::fs::metadata(file).ok()?.modified().ok()?)?;
        Some(if cfg!(windows) { format!("http://grapple.localhost/art/{id}/{kind}?v={v}") } else { format!("grapple://art/{id}/{kind}?v={v}") })
    }

    /// The Epic catalog namespace of an owned game, from legendary's metadata (`asset_infos.<platform>.namespace`).
    fn epic_namespace(legendary: &Path, app_name: &str) -> Option<String> {
        if !app_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return None;
        }
        let text = std::fs::read_to_string(legendary.join("metadata").join(format!("{app_name}.json"))).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        v.get("asset_infos")?.as_object()?.values().find_map(|p| p.get("namespace")?.as_str().map(str::to_string)).or_else(|| v.pointer("/metadata/namespace")?.as_str().map(str::to_string))
    }

    /// The tether snapshot's friends and what they are playing. `playing.game` is a library id (`steam:730`) or
    /// `tether:steam:<appid>` for a Steam game that is not in the library; other games have no app id.
    pub fn friends_from_snapshot(snap: &Value) -> Vec<Friend> {
        snap.get("friends")
            .and_then(Value::as_array)
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| {
                        let playing = f.get("playing").filter(|p| !p.is_null());
                        let game = playing.and_then(|p| p.get("game")).and_then(Value::as_str).unwrap_or("");
                        let appid = game.strip_prefix("tether:").unwrap_or(game).strip_prefix("steam:").and_then(|s| s.parse().ok());
                        Some(Friend {
                            id: f.get("id")?.as_str()?.to_string(),
                            name: f.get("name")?.as_str()?.to_string(),
                            avatar: f.get("avatar").and_then(Value::as_str).map(str::to_string),
                            playing_appid: appid,
                            playing_name: playing.and_then(|p| p.get("name")).and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    impl AppLibrary {
        fn load_owned(&self) -> Vec<OwnedGame> {
            let games: Vec<Game> = self.db.games().unwrap_or_default();
            let now = crate::backend::now_secs();
            // When a Steam game first appeared in the library cache: the best local hint of when it was bought.
            // Everything cached in the first sync after installing Steam shares one moment, which says nothing.
            let births: Vec<(String, i64)> = self
                .steam_root
                .as_deref()
                .and_then(|root| {
                    let acct = grapple_store::local::account_id(root)?;
                    let dir = root.join(format!("userdata/{acct}/config/librarycache"));
                    Some(
                        std::fs::read_dir(dir)
                            .ok()?
                            .flatten()
                            .filter_map(|e| {
                                let name = e.file_name().to_str()?.strip_suffix(".json")?.to_string();
                                let m = e.metadata().ok()?;
                                Some((name, secs(m.created().or_else(|_| m.modified()).ok()?)?))
                            })
                            .collect(),
                    )
                })
                .unwrap_or_default();
            let first = births.iter().map(|b| b.1).min().unwrap_or(0);
            games
                .iter()
                .filter(|g| matches!(g.store, Store::Steam | Store::Epic))
                .map(|g| {
                    let total = self.db.stats(&g.id, g.store_playtime_min, now).map(|s| s.total_min).unwrap_or(g.store_playtime_min);
                    let icon = g
                        .art
                        .icon
                        .as_deref()
                        .and_then(|p| art_url(&g.id.0, "icon", p))
                        .or_else(|| g.art.cover.as_deref().and_then(|p| art_url(&g.id.0, "cover", p)));
                    let added_at = births.iter().find(|(n, _)| *n == g.id.store_id()).map(|b| b.1).filter(|t| *t > first + 3 * 86_400);
                    OwnedGame {
                        id: g.id.0.clone(),
                        name: g.name.clone(),
                        playtime_min: total,
                        last_played: g.last_played,
                        added_at,
                        icon,
                        epic_namespace: if g.store == Store::Epic { self.legendary_dir.as_deref().and_then(|l| epic_namespace(l, g.id.store_id())) } else { None },
                    }
                })
                .collect()
        }
    }

    impl Library for AppLibrary {
        fn owned(&self) -> Vec<OwnedGame> {
            let mut guard = self.owned.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, v)) = guard.as_ref()
                && at.elapsed() < OWNED_TTL
            {
                return v.clone();
            }
            let v = self.load_owned();
            *guard = Some((Instant::now(), v.clone()));
            v
        }

        fn friends(&self) -> Vec<Friend> {
            let Some(state) = self.app.try_state::<Arc<crate::AppState>>() else { return Vec::new() };
            state.backend.tether_status().map(|snap| friends_from_snapshot(&snap)).unwrap_or_default()
        }

        fn steam_id64(&self) -> Option<u64> {
            Some(grapple_store::steam::steam_id64(grapple_store::local::account_id(self.steam_root.as_deref()?)?))
        }

        fn steam_region(&self) -> Option<(String, &'static str)> {
            grapple_store::local::detect_country(self.steam_root.as_deref()?)
        }
    }

    // --------------------------------------------------------------------------------------------- setup

    fn key_store() -> Arc<dyn KeyStore> {
        if std::env::var_os("GRAPPLE_NO_KEYRING").is_some() {
            return Arc::new(MemoryKeys::new());
        }
        match KeyringKeys::new() {
            Ok(k) => Arc::new(k),
            Err(e) => {
                tracing::warn!("store: no keyring ({e}); the IsThereAnyDeal key will not be kept");
                Arc::new(MemoryKeys::new())
            }
        }
    }

    fn money_text(m: &grapple_store::Money) -> String {
        let major = m.minor as f64 / 10f64.powi(grapple_store::money::decimals(&m.currency) as i32);
        let sym = match m.currency.as_str() {
            "ZAR" => "R",
            "USD" => "$",
            "EUR" => "€",
            "GBP" => "£",
            other => other,
        };
        format!("{sym} {major:.2}")
    }

    /// A desktop notification through `notify-send`, started with the session environment (not Grapple's own).
    /// Used for sales and for "<Game> is ready" (downloads).
    /// While a game runs with "Nothing while I'm playing" on, it waits ([`crate::quiet`]).
    pub fn notify_desktop(title: String, body: String) {
        let now = crate::quiet::gate().submit(title, body);
        if let Some((title, body)) = now {
            send_desktop(title, body);
        }
    }

    /// Show it now, whatever the quiet setting says (held notifications when the game closes).
    pub fn send_desktop(title: String, body: String) {
        if std::env::var_os("GRAPPLE_NO_NOTIFY").is_some() {
            return;
        }
        #[cfg(not(target_os = "windows"))]
        std::thread::spawn(move || {
            let r = std::process::Command::new("notify-send")
                .args(["-a", "grapple", "-u", "normal", "--"])
                .arg(title)
                .arg(body)
                .env_clear()
                .envs(grapple_launch::env::for_child())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if let Err(e) = r {
                tracing::debug!("notify-send: {e}");
            }
        });
        // A Windows toast. The installer's Start menu shortcut carries grapple's app id (the bundle identifier); a
        // build that was not installed shows its toasts as PowerShell's, as tauri-plugin-notification does.
        // untested on Windows: the toast's app id must match the shortcut's (Tauri's NSIS installer sets it).
        #[cfg(target_os = "windows")]
        std::thread::spawn(move || {
            use tauri_winrt_notification::Toast;
            let app_id = if cfg!(debug_assertions) { Toast::POWERSHELL_APP_ID } else { "dev.grapple.app" };
            if let Err(e) = Toast::new(app_id).title(&title).text1(&body).show() {
                tracing::debug!("toast: {e}");
            }
        });
    }

    fn notify(alert: &SaleAlert) {
        let store = match alert.store {
            StoreKind::Steam => "Steam",
            StoreKind::Epic => "Epic",
        };
        let title = if alert.final_price.minor == 0 { format!("{} is free", alert.name) } else { format!("{} is on sale", alert.name) };
        let body = if alert.final_price.minor == 0 { format!("On {store}") } else { format!("-{}% on {store} · {}", alert.discount_pct, money_text(&alert.final_price)) };
        notify_desktop(title, body);
    }

    /// Josh's Epic login (legendary's `user.json`, refreshed under legendary's lock with the rotated tokens
    /// written back) as the store's Epic session. The token goes to the store crate only to be put in a request
    /// header; nothing here logs it.
    struct EpicLogin {
        auth: grapple_launch::epic_auth::EpicAuth,
        user_json: PathBuf,
    }

    impl EpicLogin {
        async fn current(&self) -> grapple_store::Result<EpicSession> {
            let (who, token) = self.auth.session().await.map_err(|e| grapple_store::Error::Unavailable(format!("Epic login: {e}")))?;
            Ok(EpicSession { account_id: who.account_id, token: grapple_store::secrets::ApiKey::new(token.expose()) })
        }
    }

    impl EpicSessionProvider for EpicLogin {
        fn available(&self) -> bool {
            self.user_json.is_file()
        }
        fn session(&self) -> futures_util::future::BoxFuture<'_, grapple_store::Result<EpicSession>> {
            Box::pin(self.current())
        }
        fn refresh(&self) -> futures_util::future::BoxFuture<'_, grapple_store::Result<EpicSession>> {
            Box::pin(async move {
                self.auth.ensure_session(true).await.map_err(|e| grapple_store::Error::Unavailable(format!("Epic login: {e}")))?;
                self.current().await
            })
        }
    }

    /// Create the store service and start its background work. Called once from `setup`.
    pub fn init(app: &AppHandle) {
        let steam_root = grapple_scan::ScanConfig::detect();
        let db = match Db::open_default() {
            Ok(db) => db,
            Err(e) => {
                tracing::warn!("store: no database ({e}); the store is off");
                return;
            }
        };
        let data_dir = grapple_core::db::default_db_path().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
        let lib = Arc::new(AppLibrary { app: app.clone(), db, steam_root: steam_root.steam_root, legendary_dir: steam_root.legendary_dir, owned: Mutex::new(None) });
        let cfg = StoreConfig::real(data_dir, cache_dir().join("store-art"));
        let emit_app = app.clone();
        let emit = Arc::new(move |ev: StoreEvent| match ev {
            StoreEvent::Changed(c) => {
                let _ = emit_app.emit("store-changed", &c);
            }
            StoreEvent::Sale(a) => {
                notify(&a);
                let _ = emit_app.emit("store-sale", &a);
            }
        });
        let handle = tauri::async_runtime::handle().inner().clone();
        match StoreService::new(cfg, lib, key_store(), emit, handle) {
            Ok(s) => {
                // Epic prices with the Epic login Grapple already uses to launch Epic games (read-only calls).
                let user_json = grapple_launch::epic_auth::default_user_json();
                match grapple_launch::epic_auth::EpicAuth::new(grapple_launch::epic_auth::EpicApi::default(), user_json.clone()) {
                    Ok(auth) => s.enable_epic_account(Arc::new(EpicLogin { auth, user_json })),
                    Err(e) => tracing::debug!("store: no Epic login for prices ({e})"),
                }
                s.start();
                app.manage(s);
            }
            Err(e) => tracing::warn!("store: could not start ({e}); the store is off"),
        }
    }

    fn service<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<StoreService, String> {
        app.try_state::<StoreService>().map(|s| s.inner().clone()).ok_or_else(|| "The store is not available".to_string())
    }

    fn val<T: serde::Serialize>(r: grapple_store::Result<T>) -> Res {
        r.map_err(err).and_then(|v| serde_json::to_value(v).map_err(err))
    }

    /// One entry point for the command wrappers: `what` is the command name without the `store_` prefix.
    pub async fn call(app: &AppHandle, what: &str, a: Value) -> Res {
        let s = service(app)?;
        let key = || a.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        let on = || a.get("on").and_then(Value::as_bool).unwrap_or(false);
        match what {
            "front" => val(s.front().await),
            "game" => val(s.game(&key()).await),
            "under" => val(s.under(a.get("tierMinor").and_then(Value::as_i64).unwrap_or(0), a.get("includeHidden").and_then(Value::as_bool).unwrap_or(false)).await),
            "new_releases" => val(s.new_releases(a.get("includeHidden").and_then(Value::as_bool).unwrap_or(false)).await),
            "search" => {
                let filters: StoreFilters = serde_json::from_value(a.get("filters").cloned().unwrap_or(Value::Null)).unwrap_or_default();
                val(s.search(a.get("q").and_then(Value::as_str).unwrap_or(""), filters).await)
            }
            "wishlist" => val(s.wishlist().await),
            "wishlist_set" => val(s.wishlist_set(&key(), on()).await),
            "wishlist_hidden" => val(s.wishlist_hidden().await),
            "wishlist_unhide" => val(s.wishlist_unhide().await),
            "notify_set" => val(s.notify_set(&key(), on()).await),
            "home_picks" => val(s.home_picks().await),
            "settings_get" => val(Ok(s.settings_get())),
            "settings_set" => {
                let settings: StoreSettings = serde_json::from_value(a.get("settings").cloned().unwrap_or(Value::Null)).map_err(err)?;
                val(s.settings_set(settings))
            }
            "currencies" => val(Ok(s.currencies())),
            "region_get" => val(Ok(s.region())),
            "region_set" => val(s.region_set(a.get("country").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string))),
            "itad_key_set" => val(s.itad_key_set(a.get("key").and_then(Value::as_str).map(str::to_string)).await),
            "buy_target" => {
                let store = match a.get("store").and_then(Value::as_str) {
                    Some("epic") => StoreKind::Epic,
                    _ => StoreKind::Steam,
                };
                val(s.buy_target(&key(), store))
            }
            other => Err(format!("unknown store call {other}")),
        }
        .inspect_err(|e| tracing::debug!("store {what}: {e}"))
    }

    pub async fn art<R: tauri::Runtime>(app: &AppHandle<R>, key: &str, kind: &str) -> super::Served {
        use super::Served;
        let Ok(s) = service(app) else { return Served::Unavailable };
        match s.art(key, kind).await {
            Ok(Some(f)) => Served::File { path: f.path, mime: f.mime, mtime: f.mtime, len: f.len },
            Ok(None) => Served::Missing,
            Err(grapple_store::Error::Invalid(_)) => Served::Bad,
            Err(e) => {
                tracing::debug!("store art {key}/{kind}: {e}");
                Served::Unavailable
            }
        }
    }
}

#[cfg(all(test, feature = "real"))]
mod tests {
    use super::real::*;
    use serde_json::json;

    #[test]
    fn friends_and_their_games_come_out_of_the_tether_snapshot() {
        let snap = json!({"friends": [
            {"id": "1", "name": "rowan", "avatar": "grapple://avatar/1?v=a", "playing": {"game": "steam:730", "name": "Counter-Strike 2"}},
            {"id": "2", "name": "kai", "avatar": null, "playing": {"game": "tether:steam:548430", "name": "Deep Rock Galactic"}},
            {"id": "3", "name": "mia", "playing": {"game": "tether:some game", "name": "Some Game"}},
            {"id": "4", "name": "zed", "playing": null},
            {"name": "no id"}
        ]});
        let f = friends_from_snapshot(&snap);
        assert_eq!(f.len(), 4);
        assert_eq!((f[0].playing_appid, f[0].avatar.as_deref()), (Some(730), Some("grapple://avatar/1?v=a")));
        assert_eq!((f[1].playing_appid, f[1].playing_name.as_deref()), (Some(548430), Some("Deep Rock Galactic")));
        assert_eq!((f[2].playing_appid, f[2].playing_name.as_deref()), (None, Some("Some Game")));
        assert!(f[3].playing_name.is_none());
        assert!(friends_from_snapshot(&json!({})).is_empty());
    }
}
