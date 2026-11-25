//! What the Tauri commands call. Everything answers from the cache; anything stale refreshes in the background
//! and raises `store-changed`. The few calls that must wait for the network (search, a game nobody has looked
//! at yet) wait only as long as they have to, and say when they stop waiting.

use super::build::*;
use super::*;
use crate::art::{ArtFile, Key};
use crate::epic::EpicOffer;
use crate::error::Error;
use crate::money::{currency_name, decimals};
use crate::reco;
use crate::steam::SearchQuery;
use crate::types::*;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// How long an on-demand call waits for the network before it answers with what it has.
const WAIT: Duration = Duration::from_secs(8);

/// Where "Buy" goes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuyTarget {
    pub store: StoreKind,
    /// The store page of the game at that store (Steam app page, Epic product page or the price source's link).
    pub url: String,
    /// Epic: the catalog namespace and offer id (`/purchase?offers=1-<namespace>-<offerId>`), when known.
    pub namespace: Option<String>,
    pub offer_id: Option<String>,
}

async fn spawn_blocking_res<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::Io(e.to_string()))
}

impl StoreService {
    /// Start the background work: read the key store, then keep the wishlist and the front fresh. Cheap to call
    /// again.
    pub fn start(&self) {
        self.redetect_region();
        let inner = self.inner.clone();
        // Not tracked: this loop never ends.
        self.inner.handle.spawn(async move {
            let i2 = inner.clone();
            let _ = tokio::task::spawn_blocking(move || i2.itad.refresh_key()).await;
            tokio::time::sleep(Duration::from_secs(20)).await;
            loop {
                // The front keeps fresh only while the store has been opened lately: no chatter otherwise.
                let opened = inner.db.setting::<i64>("store.last_open").ok().flatten().unwrap_or(0);
                if now_secs() - opened < 7 * 86_400 {
                    let _ = inner.refresh_front(false).await;
                }
                inner.wishlist_tick().await;
                tokio::time::sleep(Duration::from_secs(1_800)).await;
            }
        });
    }

    /// Run a full refresh now and wait for it (tests, the cold-build measurement).
    pub async fn refresh_now(&self, force: bool) -> Result<()> {
        self.inner.refresh_front(force).await
    }

    pub async fn wishlist_tick_now(&self) {
        self.inner.wishlist_tick().await;
    }

    /// Refresh in the background, at most once every two minutes (a failing refresh must not turn into a loop).
    fn kick(&self) {
        let now = now_secs();
        let last = self.inner.last_kick.load(Ordering::SeqCst);
        if now - last < 120 || self.inner.net.steam_blocked() {
            return;
        }
        self.inner.last_kick.store(now, Ordering::SeqCst);
        let inner = self.inner.clone();
        self.inner.spawn(async move {
            let _ = inner.refresh_front(false).await;
        });
    }

    // ---------------------------------------------------------------------------------------------- front

    pub async fn front(&self) -> Result<StoreFront> {
        let inner = self.inner.clone();
        let _ = inner.db.set_setting("store.last_open", &now_secs());
        let (front, stale) = spawn_blocking_res(move || {
            let ctx = inner.ctx();
            let f = inner.front(&ctx, false, None);
            let stale = f.updated_at == 0 || now_secs() - f.updated_at >= ttl::LIST;
            (f, stale)
        })
        .await?;
        if stale {
            self.kick();
        }
        Ok(front)
    }

    pub async fn home_picks(&self) -> Result<Vec<StoreGame>> {
        let inner = self.inner.clone();
        let (picks, stale) = spawn_blocking_res(move || {
            let ctx = inner.ctx();
            let tier = inner.default_tier_minor(&ctx);
            let lists = inner.lists(ctx.cc(), inner.tier_major(&ctx, tier));
            (inner.home_picks(&ctx), lists.updated_at == 0 || now_secs() - lists.updated_at >= ttl::LIST)
        })
        .await?;
        if stale {
            self.kick();
        }
        Ok(picks)
    }

    pub async fn wishlist(&self) -> Result<Vec<StoreGame>> {
        let inner = self.inner.clone();
        let i2 = self.inner.clone();
        let games = spawn_blocking_res(move || {
            let ctx = inner.ctx();
            inner.wishlist_games(&ctx, usize::MAX, false)
        })
        .await?;
        // Names and prices of what is not cached yet, then tell the UI.
        self.inner.spawn(async move {
            let mark = i2.fetch_mark();
            let _ = i2.ensure_wishlist(false).await;
            let ctx = i2.ctx();
            let ids = Inner::wish_steam_ids(&ctx);
            if !ids.is_empty() {
                let r = async {
                    i2.ensure_prices(ctx.cc(), &ids, ttl::PRICE).await?;
                    i2.ensure_details(ctx.cc(), &ids, ttl::DETAILS).await?;
                    Ok::<(), Error>(())
                }
                .await;
                if let Err(e) = r {
                    tracing::debug!("store: wishlist data: {e}");
                }
            }
            if i2.fetch_mark() != mark {
                i2.changed(&["wishlist"], true);
            }
        });
        Ok(games)
    }

    pub async fn wishlist_set(&self, key: &str, on: bool) -> Result<()> {
        let k = Key::parse(key).ok_or_else(|| Error::Invalid("not a store key".into()))?;
        let inner = self.inner.clone();
        let key = k.as_string();
        let key2 = key.clone();
        spawn_blocking_res(move || -> Result<()> {
            if on {
                let name = match Key::parse(&key2) {
                    Some(Key::Steam(id)) => inner.db.infos(&inner.region().country, &[id]).ok().and_then(|mut m| m.remove(&id)).and_then(|(i, _)| i).map(|i| i.name).unwrap_or_default(),
                    _ => inner.db.epic_offer(&key2).ok().flatten().and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok()).map(|o| o.title).unwrap_or_default(),
                };
                inner.db.wish_add_local(&key2, &name)
            } else {
                inner.db.wish_remove(&key2)
            }
        })
        .await??;
        if on && let Key::Steam(id) = k {
            let i2 = self.inner.clone();
            self.inner.spawn(async move {
                let mark = i2.fetch_mark();
                let cc = i2.region().country;
                let _ = i2.ensure_details(&cc, &[id], ttl::DETAILS).await;
                let _ = i2.ensure_prices(&cc, &[id], ttl::PRICE).await;
                if i2.fetch_mark() != mark {
                    i2.changed(&["wishlist", "front"], true);
                }
            });
        }
        self.inner.changed(&["wishlist", "front"], true);
        Ok(())
    }

    /// The Steam-wishlist games hidden from grapple's wishlist.
    pub async fn wishlist_hidden(&self) -> Result<Vec<HiddenWish>> {
        let inner = self.inner.clone();
        spawn_blocking_res(move || Ok(inner.db.wish_hidden()?.into_iter().map(|(key, name)| HiddenWish { key, name }).collect())).await?
    }

    /// Show every hidden Steam-wishlist game again; how many came back.
    pub async fn wishlist_unhide(&self) -> Result<usize> {
        let inner = self.inner.clone();
        let n = spawn_blocking_res(move || inner.db.wish_unhide_all()).await??;
        if n > 0 {
            let i2 = self.inner.clone();
            self.inner.spawn(async move {
                let mark = i2.fetch_mark();
                let ctx = i2.ctx();
                let ids = Inner::wish_steam_ids(&ctx);
                if let Err(e) = async {
                    i2.ensure_prices(ctx.cc(), &ids, ttl::PRICE).await?;
                    i2.ensure_details(ctx.cc(), &ids, ttl::DETAILS).await?;
                    Ok::<(), Error>(())
                }
                .await
                {
                    tracing::debug!("store: unhidden wishlist data: {e}");
                }
                if i2.fetch_mark() != mark {
                    i2.changed(&["wishlist", "front"], true);
                }
            });
            self.inner.changed(&["wishlist", "front"], true);
        }
        Ok(n)
    }

    pub async fn notify_set(&self, key: &str, on: bool) -> Result<()> {
        let k = Key::parse(key).ok_or_else(|| Error::Invalid("not a store key".into()))?.as_string();
        let inner = self.inner.clone();
        spawn_blocking_res(move || {
            if on {
                // Re-arm: the next sale alerts again.
                let _ = inner.db.clear_sale_seen(&k);
                let _ = inner.db.clear_sale_seen(&format!("{k}|epic"));
            }
            inner.db.wish_set_notify(&k, on)
        })
        .await??;
        self.inner.changed(&["wishlist"], true);
        Ok(())
    }

    // ---------------------------------------------------------------------------------------- one game

    pub async fn game(&self, key: &str) -> Result<StoreGameDetail> {
        let k = Key::parse(key).ok_or_else(|| Error::Invalid("not a store key".into()))?;
        match k {
            Key::Steam(id) => {
                // Details, price, tags and the Epic price, together; each is skipped when fresh.
                let inner = self.inner.clone();
                let cc = inner.region().country;
                let fetch = {
                    let inner = inner.clone();
                    let cc = cc.clone();
                    self.inner.spawn(async move {
                        let ids = [id];
                        // Tags after the details: the bulk details bring them along.
                        let details = async {
                            inner.ensure_details(&cc, &ids, ttl::DETAILS).await?;
                            inner.ensure_tags(&ids).await
                        };
                        let (a, b, d) = tokio::join!(details, inner.ensure_prices(&cc, &ids, ttl::PRICE), inner.ensure_quotes(&ids, ttl::QUOTE));
                        for r in [a, b, d] {
                            if let Err(e) = r {
                                tracing::debug!("store: game {id}: {e}");
                            }
                        }
                    })
                };
                let _ = tokio::time::timeout(WAIT, fetch).await;
                let inner = self.inner.clone();
                spawn_blocking_res(move || {
                    let ctx = inner.ctx();
                    let tier = inner.default_tier_minor(&ctx);
                    let lists = inner.lists(ctx.cc(), inner.tier_major(&ctx, tier));
                    let wish = Inner::wish_steam_ids(&ctx);
                    let mut pool = Inner::pool(&lists, &wish);
                    if !pool.contains(&id) {
                        pool.push(id);
                    }
                    let b = inner.batch(&ctx, &pool, lists.names());
                    if b.info(id).is_none() && !b.names.contains_key(&id) {
                        return Err(Error::NotFound);
                    }
                    let game = inner.steam_game(&ctx, &b, id, true);
                    // More like this: the closest by tags among what the store knows, vetted like any section.
                    let me = Inner::tag_vec_pub(&b, id);
                    let o = Opts { lenient: false, released_only: true };
                    let mut like: Vec<(u32, f64)> = pool
                        .iter()
                        .copied()
                        .filter(|&c| c != id && b.info(c).is_some() && matches!(inner.keep(&ctx, &b, c, o), Keep::Show))
                        .map(|c| (c, reco::cosine(&me, &Inner::tag_vec_pub(&b, c))))
                        .filter(|(_, s)| *s > 0.0)
                        .collect();
                    like.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    let more = like.into_iter().take(4).map(|(c, _)| inner.steam_game(&ctx, &b, c, true)).collect();
                    Ok(StoreGameDetail { game, more_like_this: more })
                })
                .await?
            }
            Key::Epic { .. } => {
                let inner = self.inner.clone();
                let key = k.as_string();
                spawn_blocking_res(move || {
                    let ctx = inner.ctx();
                    let o = inner.db.epic_offer(&key).ok().flatten().and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok()).ok_or(Error::NotFound)?;
                    Ok(StoreGameDetail { game: inner.epic_game(&ctx, &o), more_like_this: Vec::new() })
                })
                .await?
            }
        }
    }

    // ----------------------------------------------------------------------------------------- the rows

    /// The "Under" row for a tier, fetching that tier's list when it has not been seen (waits a few seconds
    /// for the first games to be vetted, then answers with what it has).
    pub async fn under(&self, tier_minor: i64, include_hidden: bool) -> Result<Under> {
        if tier_minor <= 0 || tier_minor > 100_000_000 {
            return Err(Error::Invalid("bad price".into()));
        }
        let inner = self.inner.clone();
        let _ = inner.db.set_setting("store.tier", &tier_minor);
        let work = {
            let inner = inner.clone();
            self.inner.spawn(async move {
                let mark = inner.fetch_mark();
                let ctx = inner.ctx();
                let cc = ctx.cc().to_string();
                let major = inner.tier_major(&ctx, tier_minor);
                if let Err(e) = inner.ensure_lists(&cc, major, false).await {
                    tracing::debug!("store: under: {e}");
                    return;
                }
                let lists = inner.lists(&cc, major);
                let ids: Vec<u32> = lists.under.iter().map(|i| i.appid).filter(|i| !ctx.owned_steam.contains(i)).collect();
                let r = async {
                    inner.ensure_prices(&cc, &ids, ttl::PRICE).await?;
                    let first: Vec<u32> = ids.iter().copied().take(24).collect();
                    inner.ensure_details(&cc, &first, ttl::DETAILS).await?;
                    if ctx.s.hide_adult {
                        inner.ensure_tags(&first).await?;
                    }
                    Ok::<(), Error>(())
                }
                .await;
                if let Err(e) = r {
                    tracing::debug!("store: under: {e}");
                }
                if inner.fetch_mark() != mark {
                    inner.changed(&["front"], true);
                }
            })
        };
        let _ = tokio::time::timeout(WAIT, work).await;
        spawn_blocking_res(move || {
            let ctx = inner.ctx();
            let lists = inner.lists(ctx.cc(), inner.tier_major(&ctx, tier_minor));
            let b = inner.batch(&ctx, &lists.under.iter().map(|i| i.appid).collect::<Vec<_>>(), lists.names());
            inner.under_section(&ctx, &lists, &b, tier_minor, Opts { lenient: false, released_only: true }, include_hidden)
        })
        .await
    }

    /// The "Popular new releases" row; `include_hidden` is its "Show". Answers from the cache (the row's games were
    /// fetched and judged by the refresh), and refreshes in the background when the lists are stale.
    pub async fn new_releases(&self, include_hidden: bool) -> Result<NewReleases> {
        let inner = self.inner.clone();
        let (section, stale) = spawn_blocking_res(move || {
            let ctx = inner.ctx();
            let tier = inner.default_tier_minor(&ctx);
            let lists = inner.lists(ctx.cc(), inner.tier_major(&ctx, tier));
            let b = inner.batch(&ctx, &lists.new.iter().map(|i| i.appid).collect::<Vec<_>>(), lists.names());
            (inner.new_section(&ctx, &lists, &b, Opts { lenient: false, released_only: true }, include_hidden), lists.updated_at == 0 || now_secs() - lists.updated_at >= ttl::LIST)
        })
        .await?;
        if stale {
            self.kick();
        }
        Ok(section)
    }

    pub async fn search(&self, q: &str, f: StoreFilters) -> Result<Vec<StoreGame>> {
        let q = q.trim().chars().take(80).collect::<String>();
        let genre = f.genre.clone().filter(|g| !g.trim().is_empty());
        let tag = f.tag.clone().filter(|g| !g.trim().is_empty());
        if q.is_empty() && genre.is_none() && tag.is_none() && f.on_sale != Some(true) {
            return Ok(Vec::new());
        }
        let inner = self.inner.clone();
        let (cc, s) = (inner.region().country, inner.settings());
        let include_hidden = f.include_hidden == Some(true);

        let mut steam_ids: Vec<u32> = Vec::new();
        let mut via_tag = false;
        if s.stores.steam && f.store != Some(StoreKind::Epic) {
            let tag_id = genre.as_deref().and_then(genre_tag_id);
            via_tag = tag_id.is_some();
            let term = match (&tag_id, &genre, q.is_empty()) {
                (None, Some(g), true) => Some(g.clone()),
                (_, _, false) => Some(q.clone()),
                _ => tag.clone().filter(|_| q.is_empty()),
            };
            let query = SearchQuery { term, filter: Some("topsellers"), tags: tag_id.into_iter().collect(), specials: f.on_sale == Some(true), count: 40, ..Default::default() };
            let key = format!("search:{cc}:{}", query_key(&query));
            let cached = inner.db.kv_get::<Vec<crate::steam::ListItem>>(&key).ok().flatten().filter(|(_, at)| now_secs() - at < ttl::LIST);
            let items = match cached {
                Some((v, _)) => v,
                None => {
                    let v = inner.steam.search(&query, &cc).await?;
                    inner.db.kv_set(&key, &v)?;
                    v
                }
            };
            // Names for games shown before their details arrive.
            let _ = inner.db.kv_set(&format!("searchnames:{cc}"), &items.iter().map(|i| (i.appid, i.name.clone())).collect::<Vec<_>>());
            steam_ids = items.iter().map(|i| i.appid).collect();
            let ids = steam_ids.clone();
            let i2 = inner.clone();
            let cc2 = cc.clone();
            let hide = s.hide_adult;
            let owned: HashSet<u32> = inner.lib.owned().iter().filter_map(OwnedGame::steam_appid).collect();
            let work = self.inner.spawn(async move {
                let r = async {
                    i2.ensure_prices(&cc2, &ids, ttl::PRICE).await?;
                    let first: Vec<u32> = ids.iter().copied().filter(|i| !owned.contains(i)).take(16).collect();
                    i2.ensure_details(&cc2, &first, ttl::DETAILS).await?;
                    if hide {
                        i2.ensure_tags(&first).await?;
                    }
                    Ok::<(), Error>(())
                }
                .await;
                if let Err(e) = r {
                    tracing::debug!("store: search: {e}");
                }
            });
            let _ = tokio::time::timeout(Duration::from_secs(12), work).await;
        }

        let names: HashMap<u32, String> = inner.db.kv_get::<Vec<(u32, String)>>(&format!("searchnames:{cc}")).ok().flatten().map(|(v, _)| v.into_iter().collect()).unwrap_or_default();
        let ql = q.to_lowercase();
        spawn_blocking_res(move || {
            let ctx = inner.ctx();
            let b = inner.batch(&ctx, &steam_ids, names);
            let o = Opts { lenient: false, released_only: false };
            let (ids, _) = inner.select(&ctx, &b, steam_ids.iter().copied(), 40, o, include_hidden);
            let mut out: Vec<StoreGame> = Vec::new();
            for id in ids {
                let g = inner.steam_game(&ctx, &b, id, false);
                if !passes(&g, &f, genre.as_deref(), tag.as_deref(), via_tag, &ctx) {
                    continue;
                }
                out.push(g);
            }
            if ctx.s.stores.epic && f.store != Some(StoreKind::Steam) && !ql.is_empty() {
                for o in inner.epic_offers() {
                    if o.title.to_lowercase().contains(&ql) {
                        let g = inner.epic_game(&ctx, &o);
                        if !g.owned && passes(&g, &f, genre.as_deref(), tag.as_deref(), false, &ctx) {
                            out.push(g);
                        }
                    }
                }
            }
            out.truncate(30);
            out
        })
        .await
    }

    // ------------------------------------------------------------------------------------------ settings

    pub fn settings_get(&self) -> StoreSettings {
        self.inner.settings()
    }

    pub fn settings_set(&self, s: StoreSettings) -> Result<StoreSettings> {
        let cur = s.display_currency.trim().to_ascii_uppercase();
        let rates = self.inner.rates();
        let region_cur = self.inner.region().currency;
        let known = cur == region_cur || cur == "USD" || rates.has(&cur) || (rates.is_empty() && cur.len() == 3 && cur.bytes().all(|b| b.is_ascii_uppercase()));
        if !known {
            return Err(Error::Invalid(format!("{cur} is not a currency grapple has a rate for")));
        }
        let mut s = s;
        s.display_currency = cur;
        s.itad_key = None;
        self.inner.db.set_setting("store.settings", &s)?;
        self.inner.changed(&["settings", "front", "wishlist"], true);
        Ok(self.inner.settings())
    }

    /// The currencies the picker offers: every one with a rate, plus Steam's own.
    pub fn currencies(&self) -> Vec<CurrencyInfo> {
        let rates = self.inner.rates();
        let mut codes: Vec<String> = rates.per_usd.keys().cloned().collect();
        let region = self.inner.region().currency;
        for extra in [region, "USD".to_string()] {
            if !codes.contains(&extra) {
                codes.push(extra);
            }
        }
        let mut out: Vec<CurrencyInfo> = codes.into_iter().map(|c| CurrencyInfo { name: currency_name(&c).map_or_else(|| c.clone(), str::to_string), decimals: decimals(&c), code: c }).collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    pub fn region(&self) -> Region {
        self.inner.region()
    }

    /// Choose the Steam store country by hand (`None`: back to automatic).
    pub fn region_set(&self, country: Option<String>) -> Result<Region> {
        match country {
            Some(c) => {
                if !valid_cc(&c) {
                    return Err(Error::Invalid("a two-letter country code, like ZA".into()));
                }
                self.inner.db.set_setting("store.country", &c.to_ascii_uppercase())?;
            }
            None => self.inner.db.clear_setting("store.country")?,
        }
        self.kick();
        self.inner.changed(&["settings", "front"], true);
        Ok(self.inner.region())
    }

    /// Set or clear the IsThereAnyDeal key (kept in the system keyring).
    pub async fn itad_key_set(&self, key: Option<String>) -> Result<StoreSettings> {
        let inner = self.inner.clone();
        spawn_blocking_res(move || match key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => inner.itad.set_key(k),
            None => inner.itad.clear_key(),
        })
        .await??;
        if self.inner.itad.available() {
            self.kick();
        }
        self.inner.changed(&["settings", "front"], true);
        Ok(self.inner.settings())
    }

    // ------------------------------------------------------------------------------------------- buying

    /// The page and ids Buy needs for a game at one store.
    pub fn buy_target(&self, key: &str, store: StoreKind) -> Result<BuyTarget> {
        let k = Key::parse(key).ok_or_else(|| Error::Invalid("not a store key".into()))?;
        match (k, store) {
            (Key::Steam(id), StoreKind::Steam) => Ok(BuyTarget { store, url: format!("https://store.steampowered.com/app/{id}/"), namespace: None, offer_id: None }),
            (Key::Epic { namespace, offer }, StoreKind::Epic) => {
                let url = self
                    .inner
                    .db
                    .epic_offer(&format!("epic:{namespace}:{offer}"))?
                    .and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok())
                    .map(|o| o.url())
                    .unwrap_or_else(|| "https://store.epicgames.com/".to_string());
                Ok(BuyTarget { store, url, namespace: Some(namespace), offer_id: Some(offer) })
            }
            (Key::Steam(id), StoreKind::Epic) => {
                let q = self.inner.db.epic_quotes(&self.inner.epic_country(), &[id])?.remove(&id).and_then(|(j, _)| j).and_then(|j| serde_json::from_str::<crate::epic::EpicQuote>(&j).ok());
                match q {
                    Some(q) => Ok(BuyTarget { store, url: q.url, namespace: q.namespace, offer_id: q.offer_id }),
                    None => Err(Error::Unavailable("no Epic price for this game".into())),
                }
            }
            (Key::Epic { .. }, StoreKind::Steam) => Err(Error::Unavailable("not on Steam".into())),
        }
    }

    // ---------------------------------------------------------------------------------------------- art

    pub async fn art(&self, key: &str, kind: &str) -> Result<Option<ArtFile>> {
        self.inner.art.get(key, kind).await
    }
}

fn query_key(q: &SearchQuery) -> String {
    format!("{}|{:?}|{:?}|{}", q.term.clone().unwrap_or_default().to_lowercase(), q.tags, q.max_price, q.specials)
}

fn passes(g: &StoreGame, f: &StoreFilters, genre: Option<&str>, tag: Option<&str>, via_tag: bool, ctx: &Ctx) -> bool {
    let has = |list: &[String], want: &str| list.iter().any(|x| x.eq_ignore_ascii_case(want));
    if let Some(gn) = genre
        && !via_tag
        && !has(&g.genres, gn)
        && !has(&g.tags, gn)
    {
        return false;
    }
    if let Some(t) = tag
        && !has(&g.tags, t)
        && !has(&g.genres, t)
    {
        return false;
    }
    let best = g.prices.iter().map(|p| p.final_price.minor).min();
    if let Some(max) = f.max_price_minor
        && best.is_some_and(|b| b > max)
    {
        return false;
    }
    if f.on_sale == Some(true) && !g.prices.iter().any(|p| p.discount_pct > 0) {
        return false;
    }
    let _ = ctx;
    true
}

impl Inner {
    pub(crate) fn tag_vec_pub(b: &Batch, id: u32) -> reco::TagVec {
        let genres = b.info(id).map(|i| i.genres.clone()).unwrap_or_default();
        reco::tag_vector(&genres, b.tags.get(&id).map(|(t, _)| t.as_slice()).unwrap_or(&[]))
    }
}
