//! The background pipeline: everything that asks the network. Each step checks its TTL first, writes to the
//! cache, and raises `store-changed`. A 429 stops the run; the next run picks up where the cache ends.

use super::build::*;
use super::*;
use crate::epic::{EpicOffer, EpicQuote, SteamRef};
use crate::error::Error;
use crate::fx;
use crate::money::from_steam;
use crate::steam::{ListItem, PriceLookup, SearchQuery};
use crate::types::*;
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};

/// Details/tags/prices fetched in parallel (the limiter has the last word).
const PARALLEL: usize = 3;
/// Owned games fetched before the displayed ones, so the first picks already know the taste.
const PROFILE_FIRST: usize = 8;

struct ResetOnDrop<'a>(&'a AtomicBool);
impl Drop for ResetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn stale(at: Option<i64>, ttl: i64) -> bool {
    at.is_none_or(|t| now_secs() - t >= ttl)
}

impl Inner {
    // ------------------------------------------------------------------------------------------ small ones

    pub async fn ensure_fx(&self, force: bool) -> Result<()> {
        let at = self.db.kv_get::<fx::FxSnapshot>("fx").ok().flatten().map(|(_, at)| at);
        if !force && !stale(at, ttl::FX) {
            return Ok(());
        }
        let r = self.net.get(crate::net::Svc::Light, &self.net.endpoints.fx, &[]).await?;
        if r.status != 200 {
            return Err(Error::Status(r.status));
        }
        self.db.kv_set("fx", &fx::parse(&r.text())?)
    }

    pub async fn ensure_epic_feed(&self, force: bool) -> Result<()> {
        let cc = self.epic_country();
        let key = format!("epicfeed:{cc}");
        let at = self.db.kv_get::<Vec<EpicOffer>>(&key).ok().flatten().map(|(_, at)| at);
        if !force && !stale(at, ttl::EPIC_FEED) {
            return Ok(());
        }
        let offers = self.epic_free.fetch(&cc).await?;
        for o in &offers {
            self.db.put_epic_offer(&o.key(), &serde_json::to_string(o)?)?;
            // The feed's price is a real Epic price: keep it for "lowest seen" and wishlist alerts.
            self.db.record_price(&o.key(), "epic", &o.original.currency, o.original.minor, o.current.minor, o.discount_pct())?;
        }
        self.db.kv_set(&key, &offers)
    }

    /// Mirror Josh's public Steam wishlist (if his Steam account is known) into the wishlist table.
    pub async fn ensure_wishlist(&self, force: bool) -> Result<()> {
        let Some(sid) = self.lib.steam_id64() else { return Ok(()) };
        let at = self.db.kv_get::<u64>("wishlist_sync").ok().flatten().map(|(_, at)| at);
        if !force && !stale(at, ttl::WISHLIST) {
            return Ok(());
        }
        let items = self.steam.wishlist(sid).await?;
        let pairs: Vec<(u32, i64)> = items.iter().map(|i| (i.appid, i.added)).collect();
        let new = self.db.wish_sync_steam(&pairs)?;
        self.db.kv_set("wishlist_sync", &sid)?;
        if !new.is_empty() {
            self.changed(&["wishlist"], false);
        }
        Ok(())
    }

    // ------------------------------------------------------------------------------------------- lists

    fn put_list(&self, key: &str, items: &[ListItem]) -> Result<()> {
        self.db.kv_set(key, &items)
    }

    fn prime_prices(&self, cc: &str, lists: &[&Vec<ListItem>]) {
        let mut m = HashMap::new();
        for l in lists {
            for i in l.iter() {
                if let Some(p) = &i.price {
                    m.insert(i.appid, PriceLookup::Priced(p.clone()));
                }
            }
        }
        if let Some(p) = m.values().find_map(|p| if let PriceLookup::Priced(p) = p { Some(p.currency.clone()) } else { None }) {
            let _ = self.db.kv_set(&format!("steam_currency:{cc}"), &p);
        }
        let _ = self.db.put_prices(cc, &m);
    }

    pub async fn ensure_lists(&self, cc: &str, under_major: u32, force: bool) -> Result<()> {
        let fresh = |key: &str| -> bool { !force && !stale(self.db.kv_get::<serde_json::Value>(key).ok().flatten().map(|(_, at)| at), ttl::LIST) };
        if !fresh(&format!("featured:{cc}")) {
            let f = self.steam.featured(cc).await?;
            self.prime_prices(cc, &[&f.specials, &f.top_sellers, &f.new_releases]);
            self.db.kv_set(&format!("featured:{cc}"), &f)?;
            self.changed(&["front"], false);
        }
        let q = |filter: &'static str, sort: Option<&'static str>, max: Option<u32>, tags: Vec<u32>, count: u32| SearchQuery { filter: Some(filter), sort_by: sort, max_price: max, tags, count, ..Default::default() };
        let jobs: [(String, SearchQuery); 3] = [
            (format!("top:{cc}"), q("topsellers", None, None, vec![], 50)),
            (format!("new:{cc}"), q("popularnew", Some("Released_DESC"), None, vec![], 50)),
            (format!("under:{cc}:{under_major}"), q("topsellers", None, Some(under_major), vec![], 50)),
        ];
        for (key, query) in jobs {
            if fresh(&key) {
                continue;
            }
            let items = self.steam.search(&query, cc).await?;
            self.put_list(&key, &items)?;
            self.changed(&["front"], false);
        }
        if !fresh("mostplayed") {
            // Popularity only: a failure here is not worth stopping for.
            match self.steam.most_played().await {
                Ok(v) => self.db.kv_set("mostplayed", &v)?,
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                Err(e) => tracing::debug!("store: most played: {e}"),
            }
        }
        Ok(())
    }

    /// "Like your games" lists: the top sellers of the profile's strongest tags.
    pub async fn ensure_taste_lists(&self, ctx: &Ctx, force: bool) -> Result<()> {
        let cc = ctx.cc();
        let ids = self.taste_tag_ids(ctx);
        if ids.is_empty() {
            return Ok(());
        }
        for &tag in &ids {
            let key = format!("taglist:{cc}:{tag}");
            if !force && !stale(self.db.kv_get::<serde_json::Value>(&key).ok().flatten().map(|(_, at)| at), ttl::LIST) {
                continue;
            }
            let items = self.steam.search(&SearchQuery { filter: Some("topsellers"), tags: vec![tag], count: 30, ..Default::default() }, cc).await?;
            self.put_list(&key, &items)?;
        }
        self.db.kv_set(&format!("taglists:{cc}"), &ids)?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------- per-app data

    /// Bulk prices for ids whose cached price is missing or older than `max_age`.
    pub async fn ensure_prices(&self, cc: &str, ids: &[u32], max_age: i64) -> Result<usize> {
        let have = self.db.prices(cc, ids)?;
        let need: Vec<u32> = ids.iter().copied().filter(|i| have.get(i).is_none_or(|p| now_secs() - p.at >= max_age)).collect();
        if need.is_empty() {
            return Ok(0);
        }
        let got = self.steam.prices(&need, cc).await?;
        if let Some(PriceLookup::Priced(p)) = got.values().find(|p| matches!(p, PriceLookup::Priced(_))) {
            let _ = self.db.kv_set(&format!("steam_currency:{cc}"), &p.currency);
        }
        self.db.put_prices(cc, &got)?;
        self.changed(&["front", "prices"], false);
        Ok(got.len())
    }

    async fn fetch_detail(&self, cc: &str, id: u32) -> Result<bool> {
        if self.net.steam_blocked() {
            return Err(Error::RateLimited);
        }
        match self.steam.details(id, cc).await? {
            Some(info) => {
                self.db.put_info(cc, &info)?;
                if let Some(p) = &info.price {
                    self.db.put_prices(cc, &HashMap::from([(id, PriceLookup::Priced(p.clone()))]))?;
                }
                if !info.name.is_empty() {
                    let _ = self.db.wish_set_name(&format!("steam:{id}"), &info.name);
                }
                Ok(true)
            }
            None => {
                self.db.put_info_missing(cc, id)?;
                Ok(false)
            }
        }
    }

    /// Steam's tag id -> name table (one request a week; an old copy beats none).
    async fn tag_names(&self) -> HashMap<u32, String> {
        let cached = self.db.kv_get::<HashMap<u32, String>>("tag_names").ok().flatten();
        if let Some((m, at)) = &cached
            && !stale(Some(*at), ttl::TAGS)
        {
            return m.clone();
        }
        match self.steam.tag_names().await {
            Ok(m) => {
                let _ = self.db.kv_set("tag_names", &m);
                m
            }
            Err(e) => {
                tracing::debug!("store: tag names: {e}");
                cached.map(|c| c.0).unwrap_or_default()
            }
        }
    }

    /// Details and community tags of many apps in one request each (`GetItems`, 50 apps per request). Returns how
    /// many were stored and the ids Steam did not describe (for the per-app fallback). Tags whose names are not
    /// known stay unwritten, so `ensure_tags` takes them from the store page.
    async fn fetch_items(&self, cc: &str, ids: &[u32]) -> Result<(usize, Vec<u32>)> {
        if self.net.steam_blocked() {
            return Err(Error::RateLimited);
        }
        let names = self.tag_names().await;
        let currency = self.db.kv_get::<String>(&format!("steam_currency:{cc}")).ok().flatten().map(|(c, _)| c);
        let mut stored = 0;
        let mut missing = Vec::new();
        for chunk in ids.chunks(crate::steam::ITEMS_CHUNK) {
            let a = self.steam.items(chunk, cc, &names, currency.as_deref()).await?;
            for info in &a.infos {
                self.db.put_info(cc, info)?;
                if let Some(p) = &info.price {
                    self.db.put_prices(cc, &HashMap::from([(info.appid, PriceLookup::Priced(p.clone()))]))?;
                }
                if !info.name.is_empty() {
                    let _ = self.db.wish_set_name(&format!("steam:{}", info.appid), &info.name);
                }
                if let Some(t) = a.tags.get(&info.appid) {
                    self.db.put_tags(info.appid, t)?;
                }
            }
            stored += a.infos.len();
            missing.extend(a.missing);
            self.changed(&["front"], false);
        }
        Ok((stored, missing))
    }

    /// Details for ids with none cached (or older than a day): bulk through `GetItems` (details and community
    /// tags together), one request per app through `appdetails` for what that does not answer. Stops at the first
    /// rate-limit answer.
    pub async fn ensure_details(&self, cc: &str, ids: &[u32], max_age: i64) -> Result<usize> {
        let have = self.db.infos(cc, ids)?;
        let mut need: Vec<u32> = ids.iter().copied().filter(|i| have.get(i).is_none_or(|(_, at)| now_secs() - at >= max_age)).collect();
        let mut n = 0;
        if !need.is_empty() {
            match self.fetch_items(cc, &need).await {
                Ok((done, missing)) => {
                    n += done;
                    need = missing;
                }
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                // The bulk service is down or changed: the old way still works.
                Err(e) => tracing::debug!("store: bulk details: {e}"),
            }
        }
        let mut first_err = None;
        let mut s = stream::iter(need.into_iter().map(|id| self.fetch_detail(cc, id))).buffer_unordered(PARALLEL);
        while let Some(r) = s.next().await {
            match r {
                Ok(_) => {
                    n += 1;
                    self.changed(&["front"], false);
                }
                Err(Error::RateLimited) => first_err = Some(Error::RateLimited),
                Err(e) => tracing::debug!("store: details: {e}"),
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(n),
        }
    }

    async fn fetch_tags(&self, id: u32) -> Result<bool> {
        if self.net.steam_blocked() {
            return Err(Error::RateLimited);
        }
        match self.steam.tags(id).await? {
            Some(t) => {
                self.db.put_tags(id, &t)?;
                Ok(true)
            }
            // No tag block (age gate, error page): remembered as "no tags" for a day so it is not hammered.
            None => {
                self.db.put_tags(id, &[])?;
                Ok(false)
            }
        }
    }

    pub async fn ensure_tags(&self, ids: &[u32]) -> Result<usize> {
        let need_of = |have: &HashMap<u32, (Vec<crate::steam::Tag>, i64)>| -> Vec<u32> {
            ids.iter()
                .copied()
                .filter(|i| match have.get(i) {
                    None => true,
                    Some((t, at)) => now_secs() - at >= if t.is_empty() { 86_400 } else { ttl::TAGS },
                })
                .collect()
        };
        let mut need = need_of(&self.db.tags(ids)?);
        let mut n = 0;
        if !need.is_empty() {
            // The bulk service gives tags with the details: one request for the lot, the store page only for
            // what it cannot name.
            match self.fetch_items(&self.region().country, &need).await {
                Ok(_) => {
                    let before = need.len();
                    need = need_of(&self.db.tags(ids)?);
                    n = before.saturating_sub(need.len());
                }
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                Err(e) => tracing::debug!("store: bulk tags: {e}"),
            }
        }
        let mut first_err = None;
        let mut s = stream::iter(need.into_iter().map(|id| self.fetch_tags(id))).buffer_unordered(PARALLEL);
        while let Some(r) = s.next().await {
            match r {
                Ok(_) => {
                    n += 1;
                    self.changed(&["front"], false);
                }
                Err(Error::RateLimited) => first_err = Some(Error::RateLimited),
                Err(e) => tracing::debug!("store: tags: {e}"),
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(n),
        }
    }

    /// Epic prices of Steam games, from the available sources in order (the Epic login first, then
    /// IsThereAnyDeal): a source's confident price wins; a game none of them prices is recorded as "no Epic price".
    pub async fn ensure_quotes(&self, ids: &[u32], max_age: i64) -> Result<usize> {
        let sources = self.epic_sources();
        if sources.is_empty() {
            return Ok(0);
        }
        let country = self.epic_country();
        let have = self.db.epic_quotes(&country, ids)?;
        let need: Vec<u32> = ids.iter().copied().filter(|i| have.get(i).is_none_or(|(_, at)| now_secs() - at >= max_age)).collect();
        if need.is_empty() {
            return Ok(0);
        }
        let cc = self.region().country;
        let infos = self.db.infos(&cc, &need)?;
        let mut remaining: Vec<SteamRef> = need
            .iter()
            .map(|&id| {
                let i = infos.get(&id).and_then(|(i, _)| i.as_ref());
                SteamRef { appid: id, name: i.map(|i| i.name.clone()).unwrap_or_default(), developers: i.map(|i| i.developers.clone()).unwrap_or_default(), publishers: i.map(|i| i.publishers.clone()).unwrap_or_default() }
            })
            .collect();
        let mut answers: HashMap<u32, Option<EpicQuote>> = HashMap::new();
        let mut last_err = None;
        for src in sources {
            if remaining.is_empty() {
                break;
            }
            match src.quotes(&country, &remaining).await {
                Ok(m) => {
                    for (id, q) in m {
                        match q {
                            Some(q) => {
                                answers.insert(id, Some(q));
                            }
                            None => {
                                answers.entry(id).or_insert(None);
                            }
                        }
                    }
                    remaining.retain(|g| !matches!(answers.get(&g.appid), Some(Some(_))));
                }
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                Err(e) => {
                    tracing::debug!("store: epic prices from {}: {e}", src.name());
                    last_err = Some(e);
                }
            }
        }
        if answers.is_empty() {
            return match last_err {
                Some(e) => Err(e),
                None => Ok(0),
            };
        }
        for (id, q) in &answers {
            match q {
                Some(q) => {
                    self.db.put_epic_quote(*id, &country, Some(&serde_json::to_string(q)?))?;
                    let _ = self.db.record_price(&format!("steam:{id}"), "epic", &q.price.currency, q.original.minor, q.price.minor, q.discount);
                }
                None => self.db.put_epic_quote(*id, &country, None)?,
            }
        }
        self.changed(&["front", "prices"], false);
        Ok(answers.len())
    }

    /// Fresh prices for the Epic offers on the wishlist (they have Epic ids, so no matching is involved).
    pub async fn ensure_epic_offer_prices(&self, max_age: i64) -> Result<usize> {
        let sources = self.epic_sources();
        if sources.is_empty() {
            return Ok(0);
        }
        let country = self.epic_country();
        let now = now_secs();
        let mut offers: Vec<crate::epic::OfferRef> = Vec::new();
        for w in self.db.wish_all()?.into_iter().filter(|w| !w.hidden) {
            let mut p = w.key.split(':');
            if let (Some("epic"), Some(ns), Some(offer), None) = (p.next(), p.next(), p.next(), p.next()) {
                let seen = self.db.kv_get::<i64>(&format!("epicprice:{}", w.key)).ok().flatten().map(|(t, _)| t);
                if seen.is_none_or(|t| now - t >= max_age) {
                    offers.push((ns.to_string(), offer.to_string()));
                }
            }
        }
        if offers.is_empty() {
            return Ok(0);
        }
        for src in sources {
            match src.offer_prices(&country, &offers).await {
                Ok(m) => {
                    let mut n = 0;
                    for ((ns, id), q) in m {
                        let Some(q) = q else { continue };
                        let key = format!("epic:{ns}:{id}");
                        // Update the stored offer (its art and title stay); the free-games feed overwrites it again when
                        // it lists the offer, which is the same price.
                        if let Some((json, _)) = self.db.epic_offer(&key)?
                            && let Ok(mut o) = serde_json::from_str::<EpicOffer>(&json)
                        {
                            o.original = q.original.clone();
                            o.current = q.price.clone();
                            self.db.put_epic_offer(&key, &serde_json::to_string(&o)?)?;
                        }
                        let _ = self.db.record_price(&key, "epic", &q.price.currency, q.original.minor, q.price.minor, q.discount);
                        self.db.kv_set(&format!("epicprice:{key}"), &now)?;
                        n += 1;
                    }
                    self.changed(&["front", "wishlist", "prices"], false);
                    return Ok(n);
                }
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                Err(e) => tracing::debug!("store: epic offer prices from {}: {e}", src.name()),
            }
        }
        Ok(0)
    }

    /// Learn which country/currency Epic bills the account in (once a day), when the Epic login can say.
    pub async fn ensure_billing(&self) {
        let account = self.account.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(a) = account
            && crate::epic::EpicPriceSource::available(a.as_ref())
        {
            if let Err(e) = a.billing().await {
                tracing::debug!("store: epic billing: {e}");
            }
        }
    }

    // ----------------------------------------------------------------------------------- what to fetch

    /// Steam ids worth fetching, most visible first, owned games left out.
    fn wanted(&self, ctx: &Ctx, lists: &Lists, wish_ids: &[u32]) -> Vec<u32> {
        let names = lists.names();
        let mut out: Vec<u32> = Vec::new();
        let mut push = |ids: Vec<u32>| out.extend(ids);
        let ids = |l: &[ListItem], n: usize| l.iter().take(n).map(|i| i.appid).collect::<Vec<_>>();
        // About what the page shows plus a few spares for what the filters take out.
        push(ids(&lists.top, 14));
        push(ids(&lists.featured.specials, 10));
        push(ids(&lists.new, 12));
        push(ids(&lists.under, 18));
        push(wish_ids.iter().copied().take(WISHLIST_STRIP_N).collect());
        push(ids(&lists.tag_lists, 12));
        let mut seen = HashSet::new();
        out.retain(|id| seen.insert(*id) && !ctx.owns_steam(*id, names.get(id).map_or("", String::as_str)));
        out
    }

    /// Prices, details and tags for what the front shows, in that order of need.
    async fn enrich(&self, force: bool) -> Result<()> {
        let ctx = self.ctx();
        let cc = ctx.cc().to_string();
        let tier = self.default_tier_minor(&ctx);
        let lists = self.lists(&cc, self.tier_major(&ctx, tier));
        let wish_ids = Inner::wish_steam_ids(&ctx);
        let price_ids: Vec<u32> = Inner::pool(&lists, &wish_ids).into_iter().filter(|i| !ctx.owned_steam.contains(i)).collect();
        self.ensure_prices(&cc, &price_ids, if force { 0 } else { ttl::PRICE }).await?;
        // With the prices known, the wishlist games on sale (the strip shows those first) get their names first.
        let wanted = self.wanted(&ctx, &lists, &self.wish_steam_ids_by_strip(&ctx));
        self.ensure_details(&cc, &wanted, if force { 0 } else { ttl::DETAILS }).await?;
        if ctx.s.hide_adult {
            let ids = self.shortlist(&ctx, tier);
            self.ensure_tags(&ids).await?;
        }
        Ok(())
    }

    /// The Steam ids the front would show if the adult filter had judged everything.
    fn shortlist(&self, ctx: &Ctx, tier: i64) -> Vec<u32> {
        let f = self.front(ctx, true, Some(tier));
        let mut ids: Vec<u32> = Vec::new();
        for g in f.featured.iter().chain(&f.picked).chain(&f.top).chain(&f.deals).chain(&f.under.games).chain(&f.new_releases.games) {
            if let Some(id) = g.key.strip_prefix("steam:").and_then(|s| s.parse().ok()) {
                ids.push(id);
            }
        }
        let mut seen = HashSet::new();
        ids.retain(|i| seen.insert(*i));
        ids
    }

    fn owned_ids_by_weight(&self, ctx: &Ctx) -> Vec<u32> {
        let mut r: Vec<(u32, f64)> = ctx.owned.iter().filter_map(|g| Some((g.steam_appid()?, crate::reco::game_weight(g, ctx.now)))).collect();
        r.sort_by(|a, b| b.1.total_cmp(&a.1));
        r.into_iter().take(PROFILE_GAMES).map(|r| r.0).collect()
    }

    async fn enrich_owned(&self, cc: &str, ids: &[u32]) -> Result<()> {
        self.ensure_details(cc, ids, ttl::DETAILS * 7).await?;
        self.ensure_tags(ids).await?;
        Ok(())
    }

    // -------------------------------------------------------------------------------------------- runs

    /// A full refresh of the store front, step by step. One at a time.
    pub async fn refresh_front(&self, force: bool) -> Result<()> {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _reset = ResetOnDrop(&self.refreshing);
        let mark = self.fetch_mark();
        let r = self.refresh_inner(force).await;
        match &r {
            Ok(()) => {}
            Err(Error::RateLimited) => tracing::warn!("store: refresh paused, the store asked for a break"),
            Err(e) => tracing::warn!("store: refresh stopped: {e}"),
        }
        if self.fetch_mark() != mark {
            self.changed(&["front"], true);
        }
        r
    }

    async fn refresh_inner(&self, force: bool) -> Result<()> {
        // Rates first: the default tier and every price on the page depend on them. Then Epic's billing country
        // (which country's Epic prices to ask for).
        self.ensure_billing().await;
        if let Err(e) = self.ensure_fx(force).await {
            tracing::debug!("store: rates: {e}");
        }
        if let Err(e) = self.ensure_epic_feed(force).await {
            tracing::debug!("store: epic feed: {e}");
        }
        let ctx = self.ctx();
        let cc = ctx.cc().to_string();
        let tier = self.default_tier_minor(&ctx);
        let major = self.tier_major(&ctx, tier);
        self.ensure_lists(&cc, major, force).await?;
        if let Err(e @ Error::RateLimited) = self.ensure_wishlist(force).await {
            return Err(e);
        }

        // The taste first, a little: so the first picks are already about Josh's games.
        let owned = self.owned_ids_by_weight(&ctx);
        let split = owned.len().min(PROFILE_FIRST);
        self.enrich_owned(&cc, &owned[..split]).await?;
        self.enrich(force).await?;

        // Then the rest of the taste, lists for his strongest tags, and what those lists brought.
        self.enrich_owned(&cc, &owned[split..]).await?;
        let ctx = self.ctx();
        self.ensure_taste_lists(&ctx, force).await?;
        self.enrich(false).await?;

        // Epic prices, when a source can give them.
        let ctx = self.ctx();
        if ctx.s.stores.epic && self.epic_source().available() {
            let tier = self.default_tier_minor(&ctx);
            let mut ids = Inner::wish_steam_ids(&ctx);
            ids.extend(self.shortlist(&ctx, tier));
            let mut seen = HashSet::new();
            ids.retain(|i| seen.insert(*i));
            ids.truncate(120);
            if let Err(e) = self.ensure_quotes(&ids, ttl::QUOTE).await {
                tracing::debug!("store: epic prices: {e}");
            }
            if let Err(e) = self.ensure_epic_offer_prices(ttl::QUOTE).await {
                tracing::debug!("store: epic offer prices: {e}");
            }
        }
        self.check_sales().await;
        Ok(())
    }

    /// The small, regular job: the wishlist's prices and the free games, so a sale is noticed even if the store
    /// page is never opened. A handful of requests.
    pub async fn wishlist_tick(&self) {
        if self.wishlist_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let _reset = ResetOnDrop(&self.wishlist_busy);
        let mark = self.fetch_mark();
        let r: Result<()> = async {
            self.ensure_billing().await;
            let _ = self.ensure_fx(false).await;
            let ctx = self.ctx();
            if ctx.s.stores.epic {
                let _ = self.ensure_epic_feed(false).await;
            }
            if ctx.s.stores.steam {
                self.ensure_wishlist(false).await?;
                let ctx = self.ctx();
                let ids = Inner::wish_steam_ids(&ctx);
                if !ids.is_empty() {
                    self.ensure_prices(ctx.cc(), &ids, ttl::WISHLIST).await?;
                    if ctx.s.stores.epic && self.epic_source().available() {
                        let _ = self.ensure_quotes(&ids, ttl::QUOTE).await;
                    }
                }
            }
            if ctx.s.stores.epic && self.epic_source().available() {
                let _ = self.ensure_epic_offer_prices(ttl::QUOTE).await;
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::debug!("store: wishlist check: {e}");
        }
        self.check_sales().await;
        if self.fetch_mark() != mark {
            self.changed(&["wishlist"], false);
        }
    }

    // ------------------------------------------------------------------------------------------ alerts

    /// Alert for wishlisted games that are on sale and were not when last seen. At most three alerts per pass
    /// (a first sync of a long wishlist must not become a wall of notifications).
    pub async fn check_sales(&self) {
        let ctx = self.ctx();
        if !ctx.s.wishlist_alerts {
            return;
        }
        let ids = Inner::wish_steam_ids(&ctx);
        let mut b = self.batch(&ctx, &ids, HashMap::new());
        // The stored copy of each wishlisted Epic offer is the freshest (the feed and the price lookups both write it).
        let feed: HashMap<String, EpicOffer> = self.epic_offers().into_iter().map(|o| (o.key(), o)).collect();
        let mut offers: HashMap<String, EpicOffer> = HashMap::new();
        for k in ctx.wish.keys().filter(|k| k.starts_with("epic:")) {
            let stored = self.db.epic_offer(k).ok().flatten().and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok());
            if let Some(o) = stored.or_else(|| feed.get(k).cloned()) {
                offers.insert(k.clone(), o);
            }
        }
        struct Hit {
            seen_key: String,
            key: String,
            name: String,
            store: StoreKind,
            pct: u32,
            fin: crate::money::Money,
            fin_minor: i64,
        }
        let mut hits: Vec<Hit> = Vec::new();
        let mut rows: Vec<&crate::db::WishRow> = ctx.wish.values().filter(|w| w.notify).collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        for w in rows {
            if let Some(id) = w.key.strip_prefix("steam:").and_then(|s| s.parse::<u32>().ok()) {
                if ctx.s.stores.steam {
                    let name = if b.name(id).is_empty() { w.name.clone() } else { b.name(id) };
                    match b.steam_price(id) {
                        Some(p) if p.discount > 0 => {
                            let fin = from_steam(p.r#final, &p.currency);
                            hits.push(Hit { seen_key: w.key.clone(), key: w.key.clone(), name: name.clone(), store: StoreKind::Steam, pct: p.discount, fin: ctx.disp(&fin), fin_minor: fin.minor });
                        }
                        _ => {
                            let _ = self.db.clear_sale_seen(&w.key);
                        }
                    }
                    if ctx.s.stores.epic {
                        let seen_key = format!("{}|epic", w.key);
                        match b.quotes.get(&id) {
                            Some(Some(q)) if q.discount > 0 => hits.push(Hit { seen_key, key: w.key.clone(), name, store: StoreKind::Epic, pct: q.discount, fin: ctx.disp(&q.price), fin_minor: q.price.minor }),
                            _ => {
                                let _ = self.db.clear_sale_seen(&seen_key);
                            }
                        }
                    }
                }
            } else if ctx.s.stores.epic
                && let Some(o) = offers.get(&w.key)
            {
                let pct = if o.free_now.is_some() { 100 } else { o.discount_pct() };
                if pct > 0 {
                    hits.push(Hit { seen_key: w.key.clone(), key: w.key.clone(), name: o.title.clone(), store: StoreKind::Epic, pct, fin: ctx.disp(&o.current), fin_minor: o.current.minor });
                } else {
                    let _ = self.db.clear_sale_seen(&w.key);
                }
            }
        }
        let mut sent = 0;
        for h in hits {
            let seen = self.db.sale_seen(&h.seen_key).ok().flatten();
            if seen.is_some_and(|s| h.fin_minor >= s) {
                continue;
            }
            let _ = self.db.set_sale_seen(&h.seen_key, h.fin_minor);
            if h.name.is_empty() {
                // A name for the notification: one details request for this game.
                if let Some(id) = h.key.strip_prefix("steam:").and_then(|s| s.parse::<u32>().ok())
                    && self.fetch_detail(ctx.cc(), id).await.is_ok()
                {
                    b = self.batch(&ctx, &ids, HashMap::new());
                }
            }
            let name = if h.name.is_empty() { b.name(h.key.strip_prefix("steam:").and_then(|s| s.parse().ok()).unwrap_or(0)) } else { h.name.clone() };
            if sent >= 3 || name.is_empty() {
                continue;
            }
            sent += 1;
            self.emit(StoreEvent::Sale(SaleAlert { key: h.key, name, store: h.store, discount_pct: h.pct, final_price: h.fin }));
        }
    }

    /// Quotes (used by tests): the cached Epic quote of an app.
    #[allow(dead_code)]
    pub fn cached_quote(&self, id: u32) -> Option<EpicQuote> {
        let country = self.epic_country();
        self.db.epic_quotes(&country, &[id]).ok()?.remove(&id)?.0.and_then(|j| serde_json::from_str(&j).ok())
    }
}
