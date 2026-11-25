//! Cache -> contract types. Nothing here touches the network.

use super::*;
use crate::art::{MAX_SHOTS, art_url};
use crate::epic::{EpicOffer, EpicQuote};
use crate::filters::{self, Verdict};
use crate::money::{Money, from_steam, nice_round, to_major};
use crate::reco::{self, Candidate, Profile};
use crate::steam::{AppInfo, Featured, ListItem, PriceLookup, SteamPrice, Tag};
use crate::types::*;
use std::collections::{HashMap, HashSet};

pub const FEATURED_N: usize = 4;
pub const PICKED_N: usize = 6;
pub const TOP_N: usize = 10;
pub const DEALS_N: usize = 8;
pub const UNDER_N: usize = 12;
pub const NEW_N: usize = 8;
pub const WISHLIST_STRIP_N: usize = 12;

/// The six genre tiles of the store front and a game whose hero stands for each.
pub const GENRE_TILES: [(&str, u32); 6] = [("Survival", 2646460), ("Co-op", 548430), ("Shooter", 553850), ("RPG", 1091500), ("Simulation", 3219030), ("Strategy", 2347080)];

/// Steam tag ids of the genre tiles (for `search?tags=`).
pub fn genre_tag_id(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "survival" => 1662,
        "co-op" => 1685,
        "shooter" => 1774,
        "rpg" => 122,
        "simulation" => 599,
        "strategy" => 9,
        "action" => 19,
        "adventure" => 21,
        "indie" => 492,
        "horror" => 1667,
        "puzzle" => 1664,
        "racing" => 699,
        "sports" => 701,
        _ => return None,
    })
}

/// Tags too generic to steer a "like this" search.
const GENERIC_TAGS: [&str; 14] = ["singleplayer", "multiplayer", "indie", "action", "adventure", "2d", "3d", "casual", "great soundtrack", "atmospheric", "story rich", "colorful", "funny", "cute"];

pub fn norm_name(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Everything read once per call: settings, rates, what Josh owns, wishlist, friends, taste.
pub(crate) struct Ctx {
    pub s: StoreSettings,
    pub region: Region,
    pub rates: Rates,
    pub owned: Vec<OwnedGame>,
    pub owned_steam: HashSet<u32>,
    pub owned_names: HashSet<String>,
    pub owned_ns: HashSet<String>,
    pub wish: HashMap<String, crate::db::WishRow>,
    pub friends: Vec<Friend>,
    pub profile: Profile,
    pub now: i64,
}

impl Ctx {
    pub fn cc(&self) -> &str {
        &self.region.country
    }

    pub fn disp(&self, m: &Money) -> Money {
        self.rates.convert_or_keep(m, &self.s.display_currency)
    }

    pub fn owns_steam(&self, id: u32, name: &str) -> bool {
        self.owned_steam.contains(&id) || (!name.is_empty() && self.owned_names.contains(&norm_name(name)))
    }
}

/// Cached lists.
#[derive(Default)]
pub(crate) struct Lists {
    pub featured: Featured,
    pub top: Vec<ListItem>,
    pub new: Vec<ListItem>,
    pub under: Vec<ListItem>,
    pub tag_lists: Vec<ListItem>,
    pub mostplayed: HashMap<u32, u64>,
    pub updated_at: i64,
}

impl Lists {
    pub fn names(&self) -> HashMap<u32, String> {
        let mut m = HashMap::new();
        for l in [&self.featured.specials, &self.featured.top_sellers, &self.featured.new_releases, &self.top, &self.new, &self.under, &self.tag_lists] {
            for i in l {
                m.entry(i.appid).or_insert_with(|| i.name.clone());
            }
        }
        m
    }
}

/// Cached facts about a set of apps.
pub(crate) struct Batch {
    pub infos: HashMap<u32, (Option<AppInfo>, i64)>,
    pub prices: HashMap<u32, crate::db::CachedPrice>,
    pub tags: HashMap<u32, (Vec<Tag>, i64)>,
    pub quotes: HashMap<u32, Option<EpicQuote>>,
    pub names: HashMap<u32, String>,
}

impl Batch {
    pub fn info(&self, id: u32) -> Option<&AppInfo> {
        self.infos.get(&id).and_then(|(i, _)| i.as_ref())
    }

    pub fn name(&self, id: u32) -> String {
        self.info(id).map(|i| i.name.clone()).or_else(|| self.names.get(&id).cloned()).unwrap_or_default()
    }

    pub fn steam_price(&self, id: u32) -> Option<SteamPrice> {
        match self.prices.get(&id).map(|p| &p.lookup) {
            Some(PriceLookup::Priced(p)) => Some(p.clone()),
            Some(PriceLookup::NoPrice) => None,
            None => self.info(id).and_then(|i| i.price.clone()),
        }
    }

    pub fn verdict(&self, id: u32) -> Verdict {
        filters::judge(self.info(id).map(|i| i.descriptors.as_slice()), self.tags.get(&id).map(|(t, _)| t.as_slice()))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Opts {
    /// Treat games the filters cannot judge yet as fine (used to work out what to fetch).
    pub lenient: bool,
    /// Drop games that are not out yet.
    pub released_only: bool,
}

pub(crate) enum Keep {
    Show,
    HiddenByFilter,
    Skip,
}

fn decode<T: serde::de::DeserializeOwned + Default>(db: &StoreDb, key: &str) -> (T, i64) {
    db.kv_get::<T>(key).ok().flatten().unwrap_or_default()
}

use crate::db::StoreDb;

impl Inner {
    pub fn ctx(&self) -> Ctx {
        let s = self.settings();
        let region = self.region();
        let rates = self.rates();
        let owned = self.lib.owned();
        let owned_steam = owned.iter().filter_map(OwnedGame::steam_appid).collect();
        let owned_names = owned.iter().map(|g| norm_name(&g.name)).filter(|n| !n.is_empty()).collect();
        let owned_ns = owned.iter().filter_map(|g| g.epic_namespace.clone()).collect();
        let wish = self.db.wish_all().unwrap_or_default().into_iter().filter(|w| !w.hidden).map(|w| (w.key.clone(), w)).collect();
        let now = now_secs();
        let profile = self.profile(&owned, &region.country, now);
        Ctx { s, region, rates, owned, owned_steam, owned_names, owned_ns, wish, friends: self.lib.friends(), profile, now }
    }

    /// The taste profile from owned Steam games whose genres (and tags) are cached.
    pub fn profile(&self, owned: &[OwnedGame], cc: &str, now: i64) -> Profile {
        let mut ranked: Vec<(&OwnedGame, u32, f64)> = owned.iter().filter_map(|g| Some((g, g.steam_appid()?, reco::game_weight(g, now)))).collect();
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
        ranked.truncate(PROFILE_GAMES);
        let ids: Vec<u32> = ranked.iter().map(|r| r.1).collect();
        let infos = self.db.infos(cc, &ids).unwrap_or_default();
        let tags = self.db.tags(&ids).unwrap_or_default();
        let items = ranked
            .into_iter()
            .map(|(g, id, _)| {
                let genres = infos.get(&id).and_then(|(i, _)| i.as_ref()).map(|i| i.genres.clone()).unwrap_or_default();
                let t = tags.get(&id).map(|(t, _)| t.as_slice()).unwrap_or(&[]);
                (g.clone(), reco::tag_vector(&genres, t))
            })
            .collect();
        reco::build_profile(items, now)
    }

    pub fn lists(&self, cc: &str, under_major: u32) -> Lists {
        let (featured, at_f): (Featured, i64) = decode(&self.db, &format!("featured:{cc}"));
        let (top, at_t): (Vec<ListItem>, i64) = decode(&self.db, &format!("top:{cc}"));
        let (new, _) = decode(&self.db, &format!("new:{cc}"));
        let (under, _) = decode(&self.db, &format!("under:{cc}:{under_major}"));
        let (mp, _): (Vec<(u32, u64)>, i64) = decode(&self.db, "mostplayed");
        let mut tag_lists = Vec::new();
        if let Ok(Some((ids, _))) = self.db.kv_get::<Vec<u32>>(&format!("taglists:{cc}")) {
            for tag in ids {
                let (l, _): (Vec<ListItem>, i64) = decode(&self.db, &format!("taglist:{cc}:{tag}"));
                tag_lists.extend(l);
            }
        }
        Lists { featured, top, new, under, tag_lists, mostplayed: mp.into_iter().collect(), updated_at: at_f.max(at_t) }
    }

    pub fn batch(&self, ctx: &Ctx, ids: &[u32], names: HashMap<u32, String>) -> Batch {
        let cc = ctx.cc();
        let epic_cc = self.epic_country();
        let quotes = self
            .db
            .epic_quotes(&epic_cc, ids)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, (json, _))| (id, json.and_then(|j| serde_json::from_str::<EpicQuote>(&j).ok())))
            .collect();
        Batch {
            infos: self.db.infos(cc, ids).unwrap_or_default(),
            prices: self.db.prices(cc, ids).unwrap_or_default(),
            tags: self.db.tags(ids).unwrap_or_default(),
            quotes,
            names,
        }
    }

    // ------------------------------------------------------------------------------------- prices

    fn steam_store_price(&self, ctx: &Ctx, id: u32, p: &SteamPrice) -> StorePrice {
        let orig = from_steam(p.initial, &p.currency);
        let fin = from_steam(p.r#final, &p.currency);
        StorePrice {
            store: StoreKind::Steam,
            original: ctx.disp(&orig),
            final_price: ctx.disp(&fin),
            discount_pct: p.discount,
            native: fin,
            url: format!("https://store.steampowered.com/app/{id}/"),
        }
    }

    fn epic_quote_price(&self, ctx: &Ctx, q: &EpicQuote) -> StorePrice {
        StorePrice { store: StoreKind::Epic, original: ctx.disp(&q.original), final_price: ctx.disp(&q.price), discount_pct: q.discount, native: q.price.clone(), url: q.url.clone() }
    }

    fn cheapest(prices: &[StorePrice]) -> Option<StoreKind> {
        let steam = prices.iter().find(|p| p.store == StoreKind::Steam)?;
        let epic = prices.iter().find(|p| p.store == StoreKind::Epic)?;
        let (s, e) = (steam.final_price.minor as f64, epic.final_price.minor as f64);
        if s < e * 0.97 {
            Some(StoreKind::Steam)
        } else if e < s * 0.97 {
            Some(StoreKind::Epic)
        } else {
            None
        }
    }

    fn lowest_seen(&self, ctx: &Ctx, key: &str) -> Option<Money> {
        let lows = self.db.history_lows(key).ok()?;
        lows.into_iter().filter_map(|(_, cur, fin)| ctx.rates.convert(fin, &cur, &ctx.s.display_currency)).min().map(|m| Money::new(m, ctx.s.display_currency.clone()))
    }

    // ------------------------------------------------------------------------------------- games

    fn steam_art(&self, id: u32, info: Option<&AppInfo>) -> StoreArt {
        let key = format!("steam:{id}");
        let u = |k: &str| Some(art_url(&key, k, self.windows_urls));
        StoreArt {
            cover: u("cover"),
            header: u("header"),
            hero: u("hero"),
            logo: u("logo"),
            shots: info.map(|i| (0..i.shots.len().min(MAX_SHOTS)).map(|n| art_url(&key, &format!("shot{n}"), self.windows_urls)).collect()).unwrap_or_default(),
        }
    }

    fn friends_of(&self, ctx: &Ctx, id: u32, name: &str) -> Vec<Similar> {
        let n = norm_name(name);
        ctx.friends
            .iter()
            .filter(|f| f.playing_appid == Some(id) || (!n.is_empty() && f.playing_name.as_deref().is_some_and(|g| norm_name(g) == n)))
            .take(3)
            .map(|f| Similar { kind: SimilarKind::Friend, id: f.id.clone(), icon: f.avatar.clone(), name: f.name.clone() })
            .collect()
    }

    fn tag_vec(b: &Batch, id: u32) -> reco::TagVec {
        let genres = b.info(id).map(|i| i.genres.clone()).unwrap_or_default();
        reco::tag_vector(&genres, b.tags.get(&id).map(|(t, _)| t.as_slice()).unwrap_or(&[]))
    }

    fn similar_for(&self, ctx: &Ctx, b: &Batch, id: u32) -> Vec<Similar> {
        let friends = self.friends_of(ctx, id, &b.name(id));
        if !friends.is_empty() {
            return friends;
        }
        ctx.profile
            .similar(&Self::tag_vec(b, id), 3)
            .into_iter()
            .map(|g| Similar { kind: SimilarKind::Game, id: g.id.clone(), icon: g.icon.clone(), name: g.name.clone() })
            .collect()
    }

    pub fn steam_game(&self, ctx: &Ctx, b: &Batch, id: u32, with_similar: bool) -> StoreGame {
        let key = format!("steam:{id}");
        let info = b.info(id);
        let name = b.name(id);
        let mut prices = Vec::new();
        if ctx.s.stores.steam
            && let Some(p) = b.steam_price(id)
        {
            prices.push(self.steam_store_price(ctx, id, &p));
        }
        if ctx.s.stores.epic
            && let Some(Some(q)) = b.quotes.get(&id)
        {
            prices.push(self.epic_quote_price(ctx, q));
        }
        let wish = ctx.wish.get(&key);
        let tags: Vec<String> = b
            .tags
            .get(&id)
            .map(|(t, _)| {
                let mut t: Vec<&Tag> = t.iter().collect();
                t.sort_by(|a, b| b.count.cmp(&a.count));
                t.into_iter().take(8).map(|x| x.name.clone()).collect()
            })
            .unwrap_or_default();
        StoreGame {
            owned: ctx.owns_steam(id, &name),
            wishlisted: wish.is_some(),
            notify_on_sale: wish.is_some_and(|w| w.notify),
            similar: if with_similar { self.similar_for(ctx, b, id) } else { Vec::new() },
            lowest_seen: self.lowest_seen(ctx, &key),
            cheapest: Self::cheapest(&prices),
            art: self.steam_art(id, info),
            genres: info.map(|i| i.genres.clone()).unwrap_or_default(),
            tags,
            short: info.map(|i| i.short.clone()).unwrap_or_default(),
            developers: info.map(|i| i.developers.clone()).unwrap_or_default(),
            release: info.and_then(|i| i.release.clone()),
            reviews: info.and_then(|i| i.reviews),
            free: info.is_some_and(|i| i.is_free),
            coming_soon: info.is_some_and(|i| i.coming_soon),
            prices,
            key,
            name,
        }
    }

    pub fn epic_game(&self, ctx: &Ctx, o: &EpicOffer) -> StoreGame {
        let key = o.key();
        let free_now = o.free_now.is_some();
        let (original, current) = (ctx.disp(&o.original), ctx.disp(&o.current));
        let price = StorePrice {
            store: StoreKind::Epic,
            original: original.clone(),
            final_price: if free_now || o.discount_pct() > 0 { current } else { original },
            discount_pct: if free_now { 100 } else { o.discount_pct() },
            native: if free_now || o.discount_pct() > 0 { o.current.clone() } else { o.original.clone() },
            url: o.url(),
        };
        let u = |k: &str| Some(art_url(&key, k, self.windows_urls));
        let wish = ctx.wish.get(&key);
        StoreGame {
            owned: ctx.owned_ns.contains(&o.namespace) || ctx.owned_names.contains(&norm_name(&o.title)),
            wishlisted: wish.is_some(),
            notify_on_sale: wish.is_some_and(|w| w.notify),
            similar: Vec::new(),
            lowest_seen: self.lowest_seen(ctx, &key),
            cheapest: None,
            art: {
                let any = o.cover.is_some() || o.thumb.is_some() || o.wide.is_some();
                let wide = o.wide.is_some() || o.cover.is_some();
                StoreArt { cover: any.then(|| u("cover")).flatten(), header: wide.then(|| u("header")).flatten(), hero: wide.then(|| u("hero")).flatten(), logo: None, shots: Vec::new() }
            },
            genres: Vec::new(),
            tags: Vec::new(),
            short: o.description.clone().unwrap_or_default(),
            developers: o.seller.clone().into_iter().collect(),
            release: None,
            reviews: None,
            free: false,
            coming_soon: false,
            prices: vec![price],
            key,
            name: o.title.clone(),
        }
    }

    // ------------------------------------------------------------------------------------- filters

    /// Whether a Steam game belongs in a section.
    pub fn keep(&self, ctx: &Ctx, b: &Batch, id: u32, o: Opts) -> Keep {
        if !ctx.s.stores.steam {
            return Keep::Skip;
        }
        let name = b.name(id);
        if ctx.owns_steam(id, &name) {
            return Keep::Skip;
        }
        if let Some(i) = b.info(id) {
            if !i.is_game() || (o.released_only && i.coming_soon) {
                return Keep::Skip;
            }
        } else if b.infos.contains_key(&id) {
            return Keep::Skip; // Steam has nothing for it
        }
        let v = b.verdict(id);
        let shown = if o.lenient {
            !(ctx.s.hide_adult && v.is_adult()) && !(ctx.s.hide_violent && v.is_violent())
        } else {
            filters::shown(v, ctx.s.hide_adult, ctx.s.hide_violent)
        };
        if shown {
            Keep::Show
        } else if (ctx.s.hide_adult && v.is_adult()) || (ctx.s.hide_violent && v.is_violent()) {
            Keep::HiddenByFilter
        } else {
            Keep::Skip // not judged yet: neither shown nor counted
        }
    }

    /// The first `limit` shown ids of a list, and how many the filters took out of the way.
    pub fn select(&self, ctx: &Ctx, b: &Batch, ids: impl IntoIterator<Item = u32>, limit: usize, o: Opts, include_hidden: bool) -> (Vec<u32>, u32) {
        let mut out = Vec::new();
        let mut hidden = 0;
        let mut seen = HashSet::new();
        for id in ids {
            if out.len() >= limit {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            match self.keep(ctx, b, id, o) {
                Keep::Show => out.push(id),
                Keep::HiddenByFilter if include_hidden => out.push(id),
                Keep::HiddenByFilter => hidden += 1,
                Keep::Skip => {}
            }
        }
        (out, hidden)
    }

    // ------------------------------------------------------------------------------------ the front

    /// Tier choice: the setting, else about 6 USD in the display currency, rounded to a number that reads well.
    pub fn default_tier_minor(&self, ctx: &Ctx) -> i64 {
        if let Ok(Some(t)) = self.db.setting::<i64>("store.tier")
            && t > 0
        {
            return t;
        }
        let cur = &ctx.s.display_currency;
        let usd6 = ctx.rates.convert(600, "USD", cur).map(|m| to_major(m, cur)).unwrap_or(6.0);
        crate::money::from_major(nice_round(usd6), cur)
    }

    /// The tier as whole units of the Steam store currency (`maxprice`).
    pub fn tier_major(&self, ctx: &Ctx, tier_minor: i64) -> u32 {
        let steam_cur = &ctx.region.currency;
        let in_steam = ctx.rates.convert(tier_minor, &ctx.s.display_currency, steam_cur).unwrap_or(tier_minor);
        to_major(in_steam, steam_cur).round().max(1.0) as u32
    }

    pub fn epic_offers(&self) -> Vec<EpicOffer> {
        let cc = self.epic_country();
        self.db.kv_get::<Vec<EpicOffer>>(&format!("epicfeed:{cc}")).ok().flatten().map(|(v, _)| v).unwrap_or_default()
    }

    /// Scores every candidate game; best first. `price_weight` > 1 favours price (Home picks).
    pub fn ranked(&self, ctx: &Ctx, b: &Batch, pool: &[u32], lists: &Lists, price_weight: f64) -> Vec<(u32, f64)> {
        let o = Opts { lenient: false, released_only: true };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for &id in pool {
            if !seen.insert(id) || b.info(id).is_none() || !matches!(self.keep(ctx, b, id, o), Keep::Show) {
                continue;
            }
            let sp = b.steam_price(id);
            let price_usd = sp
                .as_ref()
                .map(|p| ctx.rates.convert(from_steam(p.r#final, &p.currency).minor, &p.currency, "USD").map(|m| to_major(m, "USD")).unwrap_or(0.0))
                .unwrap_or(0.0);
            let name = b.name(id);
            let cand = Candidate {
                vec: Self::tag_vec(b, id),
                discount: sp.as_ref().map_or(0, |p| p.discount),
                price_usd,
                reviews: b.info(id).and_then(|i| i.reviews).or_else(|| lists.mostplayed.get(&id).copied()),
                wishlisted: ctx.wish.contains_key(&format!("steam:{id}")),
                friends: self.friends_of(ctx, id, &name).len(),
            };
            out.push((id, reco::score(&ctx.profile, &cand, price_weight)));
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Every Steam id the front draws on, in the order it matters (used for what to fetch).
    pub fn pool(lists: &Lists, wish_ids: &[u32]) -> Vec<u32> {
        let mut v: Vec<u32> = Vec::new();
        v.extend(lists.top.iter().map(|i| i.appid));
        v.extend(lists.featured.specials.iter().map(|i| i.appid));
        v.extend(lists.new.iter().map(|i| i.appid));
        v.extend(lists.under.iter().map(|i| i.appid));
        v.extend(wish_ids);
        v.extend(lists.tag_lists.iter().map(|i| i.appid));
        v.extend(lists.featured.top_sellers.iter().map(|i| i.appid));
        v.extend(lists.featured.new_releases.iter().map(|i| i.appid));
        let mut seen = HashSet::new();
        v.retain(|i| seen.insert(*i));
        v
    }

    /// The wishlist's Steam ids, newest first.
    pub fn wish_steam_ids(ctx: &Ctx) -> Vec<u32> {
        let mut rows: Vec<&crate::db::WishRow> = ctx.wish.values().filter(|w| w.key.starts_with("steam:")).collect();
        rows.sort_by(|a, b| b.added_at.cmp(&a.added_at).then_with(|| a.key.cmp(&b.key)));
        rows.into_iter().filter_map(|w| w.key.strip_prefix("steam:")?.parse().ok()).collect()
    }

    /// The wishlist's Steam ids in the order the front's strip shows them: games on sale first, then newest.
    pub fn wish_steam_ids_by_strip(&self, ctx: &Ctx) -> Vec<u32> {
        let ids = Self::wish_steam_ids(ctx);
        let prices = self.db.prices(ctx.cc(), &ids).unwrap_or_default();
        let mut sale: Vec<u32> = Vec::new();
        let mut rest: Vec<u32> = Vec::new();
        for id in ids {
            match prices.get(&id).map(|p| &p.lookup) {
                Some(PriceLookup::Priced(p)) if p.discount > 0 => sale.push(id),
                _ => rest.push(id),
            }
        }
        sale.extend(rest);
        sale
    }

    /// The whole store front from the cache. `lenient` also shows games the adult filter has not judged yet
    /// (the refresh uses it to learn what to fetch; the UI never does).
    pub fn front(&self, ctx: &Ctx, lenient: bool, tier_minor: Option<i64>) -> StoreFront {
        let tier_minor = tier_minor.unwrap_or_else(|| self.default_tier_minor(ctx));
        let major = self.tier_major(ctx, tier_minor);
        let lists = self.lists(ctx.cc(), major);
        let wish_ids = Self::wish_steam_ids(ctx);
        let pool = Self::pool(&lists, &wish_ids);
        let b = self.batch(ctx, &pool, lists.names());
        let o = Opts { lenient, released_only: true };

        let ranked = self.ranked(ctx, &b, &pool, &lists, 1.0);
        let featured_ids: Vec<u32> = ranked.iter().take(FEATURED_N).map(|r| r.0).collect();
        let picked_ids: Vec<u32> = ranked.iter().skip(FEATURED_N).take(PICKED_N).map(|r| r.0).collect();
        let (top_ids, _) = self.select(ctx, &b, lists.top.iter().map(|i| i.appid), TOP_N, o, false);

        let mut specials: Vec<&ListItem> = lists.featured.specials.iter().filter(|i| i.price.as_ref().is_some_and(|p| p.discount > 0)).collect();
        specials.sort_by(|a, b| b.price.as_ref().map(|p| p.discount).cmp(&a.price.as_ref().map(|p| p.discount)));
        let (deal_ids, _) = self.select(ctx, &b, specials.iter().map(|i| i.appid), DEALS_N, o, false);

        let under = self.under_section(ctx, &lists, &b, tier_minor, o, false);
        let new_releases = self.new_section(ctx, &lists, &b, o, false);

        let game = |id: u32, sim: bool| self.steam_game(ctx, &b, id, sim);
        let wishlist = self.wishlist_games(ctx, WISHLIST_STRIP_N, lenient);
        let epic_free = if ctx.s.stores.epic {
            let mut now: Vec<EpicFreeItem> = Vec::new();
            let mut next: Vec<EpicFreeItem> = Vec::new();
            for o in self.epic_offers() {
                let g = self.epic_game(ctx, &o);
                if g.owned {
                    continue;
                }
                if let Some(w) = &o.free_now {
                    now.push(EpicFreeItem { game: g, now: true, until: Some(w.until.clone()) });
                } else if let Some(w) = &o.free_next {
                    next.push(EpicFreeItem { game: g, now: false, until: Some(w.until.clone()) });
                }
            }
            now.extend(next);
            now
        } else {
            Vec::new()
        };
        StoreFront {
            featured: featured_ids.iter().map(|&i| game(i, true)).collect(),
            picked: picked_ids.iter().map(|&i| game(i, true)).collect(),
            top: top_ids.iter().map(|&i| game(i, false)).collect(),
            deals: deal_ids.iter().map(|&i| game(i, false)).collect(),
            under,
            new_releases,
            wishlist,
            epic_free,
            genres: GENRE_TILES.iter().map(|(n, id)| GenreTile { name: (*n).into(), art: art_url(&format!("steam:{id}"), "hero", self.windows_urls) }).collect(),
            updated_at: lists.updated_at,
        }
    }

    /// "Popular new releases"; `include_hidden` is the row's "Show" (the games the filters took out join the row).
    pub(crate) fn new_section(&self, ctx: &Ctx, lists: &Lists, b: &Batch, o: Opts, include_hidden: bool) -> NewReleases {
        let (ids, hidden) = self.select(ctx, b, lists.new.iter().map(|i| i.appid), NEW_N, o, include_hidden);
        NewReleases { games: ids.iter().map(|&i| self.steam_game(ctx, b, i, false)).collect(), hidden }
    }

    pub(crate) fn under_section(&self, ctx: &Ctx, lists: &Lists, b: &Batch, tier_minor: i64, o: Opts, include_hidden: bool) -> Under {
        let cheap = lists.under.iter().map(|i| i.appid).filter(|id| b.steam_price(*id).map(|p| ctx.disp(&from_steam(p.r#final, &p.currency)).minor <= tier_minor).unwrap_or(true));
        let (ids, hidden) = self.select(ctx, b, cheap, UNDER_N, o, include_hidden);
        Under { tier_minor, games: ids.iter().map(|&i| self.steam_game(ctx, b, i, false)).collect(), hidden }
    }

    /// Wishlist games (sale ones first, then newest). Never filtered: the wishlist is Josh's own list.
    pub fn wishlist_games(&self, ctx: &Ctx, limit: usize, _lenient: bool) -> Vec<StoreGame> {
        let steam_ids = Self::wish_steam_ids(ctx);
        let b = self.batch(ctx, &steam_ids, HashMap::new());
        let offers: HashMap<String, EpicOffer> = ctx
            .wish
            .keys()
            .filter(|k| k.starts_with("epic:"))
            .filter_map(|k| self.db.epic_offer(k).ok().flatten().and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok()).map(|o| (k.clone(), o)))
            .collect();
        let mut rows: Vec<&crate::db::WishRow> = ctx.wish.values().collect();
        rows.sort_by(|a, b| b.added_at.cmp(&a.added_at).then_with(|| a.key.cmp(&b.key)));
        let mut games: Vec<StoreGame> = rows
            .into_iter()
            .filter_map(|w| {
                if let Some(id) = w.key.strip_prefix("steam:").and_then(|s| s.parse::<u32>().ok()) {
                    if !ctx.s.stores.steam {
                        return None;
                    }
                    let mut g = self.steam_game(ctx, &b, id, false);
                    if g.name.is_empty() {
                        g.name = w.name.clone();
                    }
                    Some(g)
                } else {
                    if !ctx.s.stores.epic {
                        return None;
                    }
                    offers.get(&w.key).map(|o| self.epic_game(ctx, o))
                }
            })
            .collect();
        // Sale first; the sort is stable so the newest-first order holds inside each group.
        games.sort_by_key(|g| std::cmp::Reverse(g.prices.iter().any(|p| p.discount_pct > 0)));
        games.truncate(limit);
        games
    }

    /// Home's "Games we think you'd like": the top 5 by score, price weighed a little higher.
    pub fn home_picks(&self, ctx: &Ctx) -> Vec<StoreGame> {
        let tier = self.default_tier_minor(ctx);
        let lists = self.lists(ctx.cc(), self.tier_major(ctx, tier));
        let wish_ids = Self::wish_steam_ids(ctx);
        let pool = Self::pool(&lists, &wish_ids);
        let b = self.batch(ctx, &pool, lists.names());
        self.ranked(ctx, &b, &pool, &lists, 1.5).into_iter().take(5).map(|(id, _)| self.steam_game(ctx, &b, id, true)).collect()
    }

    /// Top tags of the taste profile that have a Steam tag id (for the "like your games" lists).
    pub fn taste_tag_ids(&self, ctx: &Ctx) -> Vec<u32> {
        let ids: Vec<u32> = ctx.owned.iter().filter_map(OwnedGame::steam_appid).collect();
        let tags = self.db.tags(&ids).unwrap_or_default();
        let mut id_of: HashMap<String, u32> = HashMap::new();
        for (t, _) in tags.values() {
            for tag in t {
                if tag.id != 0 {
                    id_of.insert(tag.name.to_lowercase(), tag.id);
                }
            }
        }
        let mut weights: Vec<(&String, &f64)> = ctx.profile.weights.iter().collect();
        weights.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
        weights
            .into_iter()
            .filter(|(t, _)| !GENERIC_TAGS.contains(&t.as_str()))
            .filter_map(|(t, _)| id_of.get(t.as_str()).copied())
            .take(3)
            .collect()
    }
}

pub const PROFILE_GAMES: usize = 20;
