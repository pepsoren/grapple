//! Every request leaves through here: one user agent, one request limiter per service, backoff on 429, and
//! counters (the cold-build request count in the report comes from them).
//!
//! Steam's store answers about 200 requests per 5 minutes before it starts refusing; the limiter keeps a
//! sliding window well below that and shares it between `store.steampowered.com` and `api.steampowered.com`.

use crate::error::{Error, Result};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;

/// Where each service lives. Tests point all of them at a local server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `https://store.steampowered.com`
    pub steam_store: String,
    /// `https://api.steampowered.com`
    pub steam_api: String,
    /// `https://shared.akamai.steamstatic.com` (art; not part of the store's request budget)
    pub steam_cdn: String,
    /// `https://store-site-backend-static.ak.epicgames.com/freeGamesPromotions`
    pub epic_free: String,
    /// `https://open.er-api.com/v6/latest/USD`
    pub fx: String,
    /// `https://api.isthereanydeal.com`
    pub itad: String,
    /// Epic's launcher store GraphQL (`<base>/graphql`)
    pub epic_graphql: String,
    /// Epic's price engine (`<base>/priceengine/api/shared/offers/price`)
    pub epic_price: String,
    /// Epic's catalog (`<base>/catalog/api/shared/bulk/offers`)
    pub epic_catalog: String,
    /// Epic's launcher service (billing account)
    pub epic_launcher: String,
}

impl Endpoints {
    pub fn real() -> Endpoints {
        Endpoints {
            steam_store: "https://store.steampowered.com".into(),
            steam_api: "https://api.steampowered.com".into(),
            steam_cdn: "https://shared.akamai.steamstatic.com".into(),
            epic_free: "https://store-site-backend-static.ak.epicgames.com/freeGamesPromotions".into(),
            fx: "https://open.er-api.com/v6/latest/USD".into(),
            itad: "https://api.isthereanydeal.com".into(),
            epic_graphql: "https://launcher.store.epicgames.com".into(),
            epic_price: "https://priceengine-public-service-ecomprod01.ol.epicgames.com".into(),
            epic_catalog: "https://catalog-public-service-prod06.ol.epicgames.com".into(),
            epic_launcher: "https://launcher-public-service-prod06.ol.epicgames.com".into(),
        }
    }

    /// Everything on one local server (`http://127.0.0.1:port`), the test layout.
    pub fn local(base: &str) -> Endpoints {
        Endpoints {
            steam_store: format!("{base}/steam-store"),
            steam_api: format!("{base}/steam-api"),
            steam_cdn: format!("{base}/cdn"),
            epic_free: format!("{base}/epic/freeGamesPromotions"),
            fx: format!("{base}/fx"),
            itad: format!("{base}/itad"),
            epic_graphql: format!("{base}/epic-gql"),
            epic_price: format!("{base}/epic-price"),
            epic_catalog: format!("{base}/epic-catalog"),
            epic_launcher: format!("{base}/epic-launcher"),
        }
    }
}

/// How hard a service may be asked.
#[derive(Debug, Clone, Copy)]
pub struct LimitConfig {
    /// At most this many requests in any `window`.
    pub max: usize,
    pub window: Duration,
    /// Requests in flight at once.
    pub concurrency: usize,
    /// At least this long between two requests starting (no bursts).
    pub min_gap: Duration,
    /// First wait after a 429; doubles per strike, capped at `backoff_max`.
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl LimitConfig {
    /// Steam: 120 per 5 minutes (the store refuses at about 200), 3 at a time.
    pub fn steam() -> LimitConfig {
        LimitConfig { max: 120, window: Duration::from_secs(300), concurrency: 3, min_gap: Duration::from_millis(350), backoff_base: Duration::from_secs(60), backoff_max: Duration::from_secs(900) }
    }

    /// Epic's static feed, the exchange-rate site, IsThereAnyDeal: a handful of requests a day.
    pub fn light() -> LimitConfig {
        LimitConfig { max: 30, window: Duration::from_secs(60), concurrency: 2, min_gap: Duration::from_millis(500), backoff_base: Duration::from_secs(30), backoff_max: Duration::from_secs(600) }
    }

    /// Art from the CDN: no window, only a cap on parallel downloads.
    pub fn cdn() -> LimitConfig {
        LimitConfig { max: 600, window: Duration::from_secs(60), concurrency: 6, min_gap: Duration::from_millis(30), backoff_base: Duration::from_secs(10), backoff_max: Duration::from_secs(120) }
    }
}

#[derive(Default)]
struct State {
    stamps: VecDeque<Instant>,
    blocked_until: Option<Instant>,
    strikes: u32,
}

pub struct Limiter {
    cfg: LimitConfig,
    state: Mutex<State>,
    slots: Semaphore,
    count: AtomicU64,
}

/// Held while a request is in flight.
pub struct Permit<'a> {
    _slot: tokio::sync::SemaphorePermit<'a>,
}

impl Limiter {
    pub fn new(cfg: LimitConfig) -> Limiter {
        Limiter { cfg, state: Mutex::new(State::default()), slots: Semaphore::new(cfg.concurrency.max(1)), count: AtomicU64::new(0) }
    }

    /// Requests made through this limiter so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Waits until a request may go out (window space, no backoff), then counts it.
    pub async fn acquire(&self) -> Permit<'_> {
        let slot = self.slots.acquire().await.expect("semaphore is never closed");
        loop {
            let wait = {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                while st.stamps.front().is_some_and(|t| now.duration_since(*t) >= self.cfg.window) {
                    st.stamps.pop_front();
                }
                let gap_wait = st.stamps.back().and_then(|t| (*t + self.cfg.min_gap).checked_duration_since(now)).filter(|d| !d.is_zero());
                match st.blocked_until {
                    Some(until) if until > now => Some(until - now),
                    _ if st.stamps.len() >= self.cfg.max => Some(st.stamps[0] + self.cfg.window - now),
                    _ if gap_wait.is_some() => gap_wait,
                    _ => {
                        st.blocked_until = None;
                        st.stamps.push_back(now);
                        None
                    }
                }
            };
            match wait {
                Some(d) => tokio::time::sleep(d.max(Duration::from_millis(1))).await,
                None => break,
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        Permit { _slot: slot }
    }

    /// The service said "too many" (429, or Steam's 403 under load): nobody asks until the wait is over.
    pub fn penalise(&self, retry_after: Option<Duration>) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.strikes = (st.strikes + 1).min(8);
        let backoff = self.cfg.backoff_base.saturating_mul(1 << (st.strikes - 1).min(6)).min(self.cfg.backoff_max);
        let wait = retry_after.map_or(backoff, |r| r.max(backoff)).min(self.cfg.backoff_max);
        st.blocked_until = Some(Instant::now() + wait);
        tracing::warn!("store: rate limited, pausing {} s", wait.as_secs());
    }

    /// A request went through: forget earlier strikes.
    pub fn succeeded(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).strikes = 0;
    }

    /// Whether a backoff is running.
    pub fn is_blocked(&self) -> bool {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.blocked_until.is_some_and(|t| t > Instant::now())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svc {
    Steam,
    Cdn,
    Light,
}

/// A response: status and body.
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Parse(e.to_string()))
    }
}

pub struct Net {
    http: reqwest::Client,
    pub endpoints: Endpoints,
    steam: Limiter,
    cdn: Limiter,
    light: Limiter,
    steam_cookie_pages: AtomicU64,
    kinds: Mutex<std::collections::BTreeMap<&'static str, u64>>,
}

/// What a cold build cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestCounts {
    pub steam: u64,
    pub cdn: u64,
    pub other: u64,
}

pub const AGE_GATE_COOKIE: &str = "birthtime=0; lastagecheckage=1-0-1990; mature_content=1; wants_mature_content=1";

impl Net {
    pub fn new(endpoints: Endpoints, steam: LimitConfig, light: LimitConfig, cdn: LimitConfig) -> Result<Net> {
        // Fails harmlessly when a provider is already installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("grapple/", env!("CARGO_PKG_VERSION"), " (game launcher)"))
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(10))
            // A redirect never carries our cookies or key to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Http(e.without_url().to_string()))?;
        Ok(Net {
            http,
            endpoints,
            steam: Limiter::new(steam),
            cdn: Limiter::new(cdn),
            light: Limiter::new(light),
            steam_cookie_pages: AtomicU64::new(0),
            kinds: Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    pub fn real() -> Result<Net> {
        Net::new(Endpoints::real(), LimitConfig::steam(), LimitConfig::light(), LimitConfig::cdn())
    }

    pub fn counts(&self) -> RequestCounts {
        RequestCounts { steam: self.steam.count(), cdn: self.cdn.count(), other: self.light.count() }
    }

    /// Tag pages fetched (each is a big HTML page).
    pub fn tag_pages(&self) -> u64 {
        self.steam_cookie_pages.load(Ordering::Relaxed)
    }

    /// Requests so far by what they asked for (`steam:details`, `steam:tag-page`, `epic:free`...).
    pub fn kinds(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.kinds.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn count_kind(&self, url: &str) {
        let k = classify(url);
        *self.kinds.lock().unwrap_or_else(|e| e.into_inner()).entry(k).or_insert(0) += 1;
    }

    pub fn steam_blocked(&self) -> bool {
        self.steam.is_blocked()
    }

    fn limiter(&self, svc: Svc) -> &Limiter {
        match svc {
            Svc::Steam => &self.steam,
            Svc::Cdn => &self.cdn,
            Svc::Light => &self.light,
        }
    }

    /// One GET. `headers` are (name, value) pairs. 429/503/403-from-Steam trip the backoff and come back as
    /// [`Error::RateLimited`]; other statuses are returned for the caller to judge.
    pub async fn get(&self, svc: Svc, url: &str, headers: &[(&str, &str)]) -> Result<Reply> {
        let limiter = self.limiter(svc);
        let _permit = limiter.acquire().await;
        self.count_kind(url);
        let mut req = self.http.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(limiter, svc, req).await
    }

    /// One POST with a JSON body.
    pub async fn post_json(&self, svc: Svc, url: &str, headers: &[(&str, &str)], body: &serde_json::Value) -> Result<Reply> {
        let limiter = self.limiter(svc);
        let _permit = limiter.acquire().await;
        self.count_kind(url);
        let mut req = self.http.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(limiter, svc, req).await
    }

    async fn send(&self, limiter: &Limiter, svc: Svc, req: reqwest::RequestBuilder) -> Result<Reply> {
        // Errors carry the request URL, which can hold a country or an id but never a key (keys travel in
        // headers); the URL is dropped anyway so nothing leaks through a log line.
        let resp = req.send().await.map_err(|e| Error::Http(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs);
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        if status == 429 || (svc == Svc::Steam && status == 403) {
            limiter.penalise(retry_after);
            return Err(Error::RateLimited);
        }
        let body = resp.bytes().await.map_err(|e| Error::Http(e.without_url().to_string()))?.to_vec();
        if status < 500 {
            limiter.succeeded();
        }
        Ok(Reply { status, body, content_type })
    }

    pub(crate) fn note_tag_page(&self) {
        self.steam_cookie_pages.fetch_add(1, Ordering::Relaxed);
    }
}

pub type SharedNet = Arc<Net>;

/// What a URL asks for, for the request counts.
fn classify(url: &str) -> &'static str {
    let has = |s: &str| url.contains(s);
    if has("search/results") {
        "steam:search"
    } else if has("featuredcategories") {
        "steam:featured"
    } else if has("appdetails") && has("filters=price_overview") {
        "steam:prices"
    } else if has("appdetails") {
        "steam:details"
    } else if has("GetItems") {
        "steam:items"
    } else if has("GetTagList") {
        "steam:tag-list"
    } else if has("GetWishlist") {
        "steam:wishlist"
    } else if has("GetMostPlayed") {
        "steam:most-played"
    } else if has("/app/") {
        "steam:tag-page"
    } else if has("freeGamesPromotions") {
        "epic:free-games"
    } else if has("graphql") || has("priceengine") || has("catalog/api") || has("billingaccounts") {
        "epic:account"
    } else if has("er-api") || has("/fx") {
        "fx"
    } else if has("isthereanydeal") || has("/itad/") {
        "itad"
    } else if has("store_item_assets") {
        "art"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: usize, secs: u64) -> LimitConfig {
        LimitConfig { max, window: Duration::from_secs(secs), concurrency: 8, min_gap: Duration::ZERO, backoff_base: Duration::from_secs(10), backoff_max: Duration::from_secs(100) }
    }

    #[tokio::test(start_paused = true)]
    async fn window_holds_requests_back_and_counts() {
        let l = Limiter::new(cfg(3, 60));
        let t0 = Instant::now();
        for _ in 0..3 {
            drop(l.acquire().await);
        }
        assert!(t0.elapsed() < Duration::from_secs(1), "the first three go straight out");
        drop(l.acquire().await);
        assert!(t0.elapsed() >= Duration::from_secs(60), "the fourth waits for the window to slide");
        assert_eq!(l.count(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn requests_are_spaced_out() {
        let l = Limiter::new(LimitConfig { min_gap: Duration::from_millis(300), ..cfg(100, 60) });
        let t0 = Instant::now();
        for _ in 0..5 {
            drop(l.acquire().await);
        }
        assert!(t0.elapsed() >= Duration::from_millis(1200), "{:?}", t0.elapsed());
        assert!(t0.elapsed() < Duration::from_millis(1500));
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_doubles_per_strike_and_resets_on_success() {
        let l = Limiter::new(cfg(100, 60));
        let t0 = Instant::now();
        l.penalise(None);
        drop(l.acquire().await);
        assert!(t0.elapsed() >= Duration::from_secs(10));
        l.penalise(None);
        let t1 = Instant::now();
        drop(l.acquire().await);
        assert!(t1.elapsed() >= Duration::from_secs(20), "second strike waits twice as long");
        l.succeeded();
        l.penalise(Some(Duration::from_secs(5)));
        let t2 = Instant::now();
        drop(l.acquire().await);
        let waited = t2.elapsed();
        assert!(waited >= Duration::from_secs(10) && waited < Duration::from_secs(20), "back to the base wait: {waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_respected_but_capped() {
        let l = Limiter::new(cfg(100, 60));
        l.penalise(Some(Duration::from_secs(50)));
        let t0 = Instant::now();
        drop(l.acquire().await);
        assert!(t0.elapsed() >= Duration::from_secs(50));
        l.penalise(Some(Duration::from_secs(10_000)));
        let t1 = Instant::now();
        drop(l.acquire().await);
        assert!(t1.elapsed() <= Duration::from_secs(101));
    }
}
