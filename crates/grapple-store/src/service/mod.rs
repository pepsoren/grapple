//! [`StoreService`]: the store's brain. It answers from the SQLite cache at once and refreshes in the
//! background, raising `store-changed` as pieces land.
//!
//! * `build.rs`  turns cached data into the contract types (filters, ownership, recommendations).
//! * `fetch.rs`  the background pipeline: lists, prices, details, tags, Epic, wishlist, sale alerts.
//! * `api.rs`    the public methods the Tauri commands call.

mod api;
mod build;
mod fetch;

use crate::art::ArtCache;
use crate::db::StoreDb;
use crate::epic::{EpicFree, EpicPriceSource, Itad, Unavailable};
use crate::epic_account::{EpicAccount, EpicSessionProvider};
use crate::error::Result;
use crate::money::{Rates, steam_currency_for};
use crate::net::{Endpoints, LimitConfig, Net};
use crate::secrets::KeyStore;
use crate::steam::Steam;
use crate::types::{Library, Region, StoreChanged, StoreEvent, StoreSettings, StoresOn};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

pub use api::*;

/// How long cached data is trusted (seconds): lists 1 h, details 24 h, prices 6 h, tags 7 days, rates 24 h.
pub mod ttl {
    pub const LIST: i64 = 3_600;
    pub const DETAILS: i64 = 86_400;
    pub const PRICE: i64 = 21_600;
    pub const TAGS: i64 = 7 * 86_400;
    pub const FX: i64 = 86_400;
    pub const EPIC_FEED: i64 = 3_600;
    pub const WISHLIST: i64 = 3_600;
    pub const QUOTE: i64 = 21_600;
}

pub struct StoreConfig {
    /// Where `store.db` lives.
    pub data_dir: PathBuf,
    /// The disk art cache.
    pub art_dir: PathBuf,
    pub endpoints: Endpoints,
    pub steam_limit: LimitConfig,
    pub light_limit: LimitConfig,
    pub cdn_limit: LimitConfig,
    /// Art URLs in the Windows form (`http://grapple.localhost/store/...`).
    pub windows_urls: bool,
}

impl StoreConfig {
    pub fn real(data_dir: PathBuf, art_dir: PathBuf) -> StoreConfig {
        StoreConfig {
            data_dir,
            art_dir,
            endpoints: Endpoints::real(),
            steam_limit: LimitConfig::steam(),
            light_limit: LimitConfig::light(),
            cdn_limit: LimitConfig::cdn(),
            windows_urls: cfg!(windows),
        }
    }
}

type Emit = Arc<dyn Fn(StoreEvent) + Send + Sync>;

pub(crate) struct Inner {
    pub db: StoreDb,
    pub net: Arc<Net>,
    pub steam: Steam,
    pub epic_free: EpicFree,
    pub itad: Arc<Itad>,
    pub extra_sources: RwLock<Vec<Arc<dyn EpicPriceSource>>>,
    pub account: RwLock<Option<Arc<EpicAccount>>>,
    pub art: ArtCache,
    pub lib: Arc<dyn Library>,
    emit: Emit,
    pub windows_urls: bool,
    refreshing: AtomicBool,
    wishlist_busy: AtomicBool,
    last_emit_ms: AtomicI64,
    last_kick: AtomicI64,
    /// Background tasks started and not finished (so tests and the cold-build measurement can wait for quiet).
    pending: AtomicUsize,
    /// What Steam's own files said when the service started (re-read by `start`).
    detected: RwLock<Option<(String, &'static str)>>,
    handle: tokio::runtime::Handle,
    me: std::sync::Weak<Inner>,
}

#[derive(Clone)]
pub struct StoreService {
    pub(crate) inner: Arc<Inner>,
}

impl StoreService {
    /// `handle` is the runtime the background work runs on.
    pub fn new(cfg: StoreConfig, lib: Arc<dyn Library>, keys: Arc<dyn KeyStore>, emit: Emit, handle: tokio::runtime::Handle) -> Result<StoreService> {
        let db = StoreDb::open(&cfg.data_dir.join("store.db"))?;
        let net = Arc::new(Net::new(cfg.endpoints, cfg.steam_limit, cfg.light_limit, cfg.cdn_limit)?);
        let itad = Arc::new(Itad::new(net.clone(), keys, db.clone()));
        let db2 = db.clone();
        let lib2 = lib.clone();
        let cc_fn: Arc<dyn Fn() -> String + Send + Sync> = {
            let db = db.clone();
            let lib = lib.clone();
            Arc::new(move || region_of(&db, lib.as_ref(), None).country)
        };
        let art = ArtCache::new(cfg.art_dir, db.clone(), net.clone(), cc_fn);
        let detected = lib2.steam_region();
        let inner = Arc::new_cyclic(|me| Inner {
            me: me.clone(),
            steam: Steam::new(net.clone()),
            epic_free: EpicFree::new(net.clone()),
            itad,
            extra_sources: RwLock::new(Vec::new()),
            account: RwLock::new(None),
            art,
            db: db2,
            net,
            lib,
            emit,
            windows_urls: cfg.windows_urls,
            refreshing: AtomicBool::new(false),
            wishlist_busy: AtomicBool::new(false),
            last_emit_ms: AtomicI64::new(0),
            last_kick: AtomicI64::new(0),
            pending: AtomicUsize::new(0),
            detected: RwLock::new(detected),
            handle,
        });
        Ok(StoreService { inner })
    }

    /// Request counts so far (the cold-build measurement).
    pub fn request_counts(&self) -> crate::net::RequestCounts {
        self.inner.net.counts()
    }

    /// Requests so far by kind.
    pub fn request_kinds(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.inner.net.kinds()
    }

    /// Tag pages fetched so far.
    pub fn tag_pages(&self) -> u64 {
        self.inner.net.tag_pages()
    }

    /// Wait until no refresh is running (tests, the cold-build measurement). `false`: still busy at the timeout.
    pub async fn wait_idle(&self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            if self.inner.pending.load(Ordering::SeqCst) == 0 && !self.inner.refreshing.load(Ordering::SeqCst) && !self.inner.wishlist_busy.load(Ordering::SeqCst) {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Read Steam's files for the country again (the app calls it when Steam may have changed them).
    pub fn redetect_region(&self) {
        *self.inner.detected.write().unwrap_or_else(|e| e.into_inner()) = self.inner.lib.steam_region();
    }

    pub fn db(&self) -> &StoreDb {
        &self.inner.db
    }

    /// Add a source of Epic prices (tried before IsThereAnyDeal). This is where an implementation on Epic's own
    /// authenticated price service plugs in.
    pub fn add_epic_source(&self, s: Arc<dyn EpicPriceSource>) {
        self.inner.extra_sources.write().unwrap_or_else(|e| e.into_inner()).insert(0, s);
    }

    /// Price Epic games with Josh's Epic login (the app's wrapper of `EpicAuth`). It becomes the first source:
    /// exact prices by offer id, and a Steam game gets an Epic price only with a confident title + maker match.
    pub fn enable_epic_account(&self, provider: Arc<dyn EpicSessionProvider>) {
        let acct = Arc::new(EpicAccount::new(self.inner.net.clone(), self.inner.db.clone(), provider));
        *self.inner.account.write().unwrap_or_else(|e| e.into_inner()) = Some(acct.clone());
        self.add_epic_source(acct);
    }

    /// Which Epic price source answers right now.
    pub fn epic_source_name(&self) -> &'static str {
        self.inner.epic_source().name()
    }
}

pub(crate) fn now_secs() -> i64 {
    crate::db::now()
}

/// Where Steam prices come from: a saved country, the environment, Steam's own files, else the US store.
pub(crate) fn region_of(db: &StoreDb, lib: &dyn Library, detected: Option<&(String, &'static str)>) -> Region {
    let pick = |c: &str, src: &str| -> Region {
        let c = c.to_ascii_uppercase();
        let currency = db.kv_get::<String>(&format!("steam_currency:{c}")).ok().flatten().map(|(v, _)| v).unwrap_or_else(|| steam_currency_for(&c).to_string());
        Region { country: c, currency, source: src.to_string() }
    };
    if let Ok(Some(c)) = db.setting::<String>("store.country")
        && valid_cc(&c)
    {
        return pick(&c, "setting");
    }
    if let Ok(c) = std::env::var("GRAPPLE_STORE_CC")
        && valid_cc(&c)
    {
        return pick(&c, "env");
    }
    let found = match detected {
        Some(d) => Some(d.clone()),
        None => lib.steam_region(),
    };
    if let Some((c, src)) = found
        && valid_cc(&c)
    {
        return pick(&c, src);
    }
    pick("US", "default")
}

pub(crate) fn valid_cc(c: &str) -> bool {
    c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic())
}

impl Inner {
    pub fn region(&self) -> Region {
        let d = self.detected.read().unwrap_or_else(|e| e.into_inner()).clone();
        region_of(&self.db, self.lib.as_ref(), d.as_ref())
    }

    /// The country for Epic's feed and prices. Josh's Epic account bills in USD, so US unless set.
    pub fn epic_country(&self) -> String {
        if let Ok(Some(c)) = self.db.setting::<String>("store.epic_country")
            && valid_cc(&c)
        {
            return c.to_ascii_uppercase();
        }
        if let Ok(c) = std::env::var("GRAPPLE_STORE_EPIC_CC")
            && valid_cc(&c)
        {
            return c.to_ascii_uppercase();
        }
        // What Epic bills the account in, once the Epic login has said.
        if let Ok(Some((b, _))) = self.db.kv_get::<crate::epic_account::Billing>("epic_billing")
            && valid_cc(&b.country)
        {
            return b.country;
        }
        "US".into()
    }

    pub fn default_settings(&self) -> StoreSettings {
        StoreSettings {
            display_currency: self.region().currency,
            hide_adult: true,
            hide_violent: false,
            stores: StoresOn { steam: true, epic: true },
            wishlist_alerts: true,
            itad_key: None,
        }
    }

    pub fn settings(&self) -> StoreSettings {
        let mut s: StoreSettings = self.db.setting("store.settings").ok().flatten().unwrap_or_else(|| self.default_settings());
        s.itad_key = Some(self.itad.available());
        s
    }

    pub fn rates(&self) -> Rates {
        self.db.kv_get::<crate::fx::FxSnapshot>("fx").ok().flatten().map(|(s, _)| s.rates()).unwrap_or_default()
    }

    /// The Epic price sources that can answer now, best first: those added (the Epic login), then ITAD.
    pub fn epic_sources(&self) -> Vec<Arc<dyn EpicPriceSource>> {
        let mut v: Vec<Arc<dyn EpicPriceSource>> = self.extra_sources.read().unwrap_or_else(|e| e.into_inner()).iter().filter(|s| s.available()).cloned().collect();
        if self.itad.available() {
            v.push(self.itad.clone());
        }
        v
    }

    /// The first source in use, else "unavailable".
    pub fn epic_source(&self) -> Arc<dyn EpicPriceSource> {
        self.epic_sources().into_iter().next().unwrap_or_else(|| Arc::new(Unavailable))
    }

    /// A number that moves whenever a request to Steam, Epic, the rates or IsThereAnyDeal went out. Work that
    /// raises `store-changed` only when it fetched something compares it before and after: the UI refetches on
    /// every `store-changed`, and a refetch that finds nothing new must not start another round.
    pub fn fetch_mark(&self) -> u64 {
        let c = self.net.counts();
        c.steam + c.other
    }

    /// Raise `store-changed`, at most about once a second unless `force`.
    pub fn changed(&self, sections: &[&str], force: bool) {
        let now = chrono::Utc::now().timestamp_millis();
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if !force && now - last < 1_000 {
            return;
        }
        self.last_emit_ms.store(now, Ordering::Relaxed);
        (self.emit)(StoreEvent::Changed(StoreChanged { sections: sections.iter().map(|s| s.to_string()).collect() }));
    }

    pub fn emit(&self, ev: StoreEvent) {
        (self.emit)(ev);
    }

    /// Run background work on the service's runtime; `wait_idle` counts it.
    pub fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: F) -> tokio::task::JoinHandle<()> {
        struct Done(Arc<Inner>);
        impl Drop for Done {
            fn drop(&mut self) {
                self.0.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.pending.fetch_add(1, Ordering::SeqCst);
        let me = self.me.upgrade().expect("the service is alive while it spawns");
        self.handle.spawn(async move {
            let _done = Done(me);
            f.await;
        })
    }
}
