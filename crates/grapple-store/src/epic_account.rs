//! Epic prices with Josh's own Epic login (the one grapple-launch already refreshes), read-only.
//!
//! Shapes and routes are in `docs/epic-prices.md`: the launcher's store GraphQL (`launcher.store.epicgames.com`)
//! for finding an offer and its price, the price engine (bulk, exact) and the catalog (fallback) for pricing an
//! offer by id. All take `Authorization: bearer <access token>`. The token comes from an
//! [`EpicSessionProvider`]; the app's one wraps `EpicAuth::session()`, which refreshes and writes rotated tokens
//! back to `user.json` under legendary's lock (Epic rotates refresh tokens: nothing in this crate ever refreshes
//! on its own). The token only ever goes into a request header.
//!
//! Matching a Steam game to an Epic offer is strict: the title must be equal after normalising case and
//! punctuation **and** a developer or publisher must match. Anything else is "no Epic price", never a guess.

use crate::db::StoreDb;
use crate::epic::{EpicOffer, EpicPriceSource, EpicQuote, OfferRef, SteamRef, parse_free_games};
use crate::error::{Error, Result};
use crate::money::{Money, decimals, from_major};
use crate::net::{Net, Svc};
use crate::secrets::ApiKey;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Who is signed in to Epic and a token to call its services with.
#[derive(Clone)]
pub struct EpicSession {
    pub account_id: String,
    pub token: ApiKey,
}

impl std::fmt::Debug for EpicSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EpicSession({}, ***)", self.account_id)
    }
}

/// The app's link to the Epic login.
pub trait EpicSessionProvider: Send + Sync + 'static {
    /// Whether an Epic login exists (a file check, no network).
    fn available(&self) -> bool;
    /// A valid session (refreshing and writing back rotated tokens when needed).
    fn session(&self) -> BoxFuture<'_, Result<EpicSession>>;
    /// An Epic service refused the token: refresh now.
    fn refresh(&self) -> BoxFuture<'_, Result<EpicSession>>;
}

/// The launcher's user agent (Epic's services expect it).
pub const LAUNCHER_UA: &str = "EpicGamesLauncher/14.0.8-22004686+++Portal+Release-Live";

const SEARCH_QUERY: &str = "query searchStoreQuery($allowCountries: String, $category: String, $count: Int, $country: String!, $keywords: String, $locale: String, $sortBy: String, $sortDir: String) { Catalog { searchStore(allowCountries: $allowCountries, category: $category, count: $count, country: $country, keywords: $keywords, locale: $locale, sortBy: $sortBy, sortDir: $sortDir) { elements { title id namespace description offerType isCodeRedemptionOnly seller { id name } productSlug urlSlug keyImages { type url } customAttributes { key value } categories { path } offerMappings { pageSlug pageType } catalogNs { mappings(pageType: \"productHome\") { pageSlug pageType } } price(country: $country) { totalPrice { discountPrice originalPrice voucherDiscount discount currencyCode currencyInfo { decimals } } lineOffers { appliedRules { id endDate discountSetting { discountType } } } } } } } }";

/// How long a match (or "no confident match") is trusted, seconds.
const MAP_TTL: i64 = 30 * 86_400;
const NO_MAP_TTL: i64 = 7 * 86_400;
/// Offers per price-engine call.
const PRICE_CHUNK: usize = 25;

// ------------------------------------------------------------------------------------------------ matching

/// Lowercase, `&` as "and", trademark signs dropped, only letters and digits kept.
pub fn norm_title(s: &str) -> String {
    s.replace('&', " and ").chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

const LEGAL: [&str; 17] = ["inc", "ltd", "llc", "gmbh", "co", "corp", "corporation", "limited", "sa", "ab", "oy", "bv", "srl", "kg", "ag", "plc", "pty"];

/// A company name as comparable words, legal suffixes ("Inc.", "GmbH") dropped.
fn maker_key(s: &str) -> String {
    let words: Vec<String> = s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !LEGAL.contains(w))
        .map(str::to_string)
        .collect();
    words.join(" ")
}

/// Whether the Steam game's developers/publishers and the offer's makers share a company.
pub fn same_maker(steam: &[String], epic: &[String]) -> bool {
    let a: HashSet<String> = steam.iter().map(|s| maker_key(s)).filter(|k| !k.is_empty()).collect();
    epic.iter().map(|s| maker_key(s)).any(|k| !k.is_empty() && a.contains(&k))
}

/// The one Epic offer that is this Steam game, or none. Equal title **and** a shared developer/publisher; several
/// editions with the same title narrow to the single base game, else nothing.
pub fn resolve(game: &SteamRef, offers: &[EpicOffer]) -> Option<EpicOffer> {
    let want = norm_title(&game.name);
    if want.is_empty() {
        return None;
    }
    let steam_makers: Vec<String> = game.developers.iter().chain(&game.publishers).cloned().collect();
    let hits: Vec<&EpicOffer> = offers.iter().filter(|o| norm_title(&o.title) == want && same_maker(&steam_makers, &o.makers)).collect();
    match hits.as_slice() {
        [one] => Some((*one).clone()),
        [] => None,
        many => {
            let base: Vec<&&EpicOffer> = many.iter().filter(|o| o.offer_type == "BASE_GAME").collect();
            match base.as_slice() {
                [one] => Some((**one).clone()),
                _ => None,
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ prices

/// Epic amounts are integers in `decimals` of the currency; Money is in ISO minor units.
fn epic_money(amount: i64, currency: &str, dec: Option<u32>) -> Money {
    let d = dec.unwrap_or_else(|| decimals(currency));
    Money::new(from_major(amount as f64 / 10f64.powi(d as i32), currency), currency)
}

fn discount_of(original: i64, current: i64) -> u32 {
    if original <= 0 || current >= original { 0 } else { (100.0 - 100.0 * current as f64 / original as f64).round() as u32 }
}

fn quote(namespace: &str, offer: &str, original: Money, price: Money, url: String) -> EpicQuote {
    EpicQuote {
        steam_appid: 0,
        discount: discount_of(original.minor, price.minor),
        original,
        price,
        url,
        history_low: None,
        namespace: Some(namespace.to_string()),
        offer_id: Some(offer.to_string()),
    }
}

/// `POST .../priceengine/api/shared/offers/price` -> offer id -> (original, current). The discount is worked
/// out from the two prices; the response's own `discountPercentage` is not trusted (docs/epic-prices.md).
pub fn parse_price_engine(text: &str) -> Result<HashMap<String, (Money, Money)>> {
    let v: Value = serde_json::from_str(text)?;
    let lines = v.get("lineOffers").and_then(Value::as_array).ok_or_else(|| Error::Parse("priceengine: no lineOffers".into()))?;
    let pay_dec = v.pointer("/totalPaymentPrice/paymentCurrencyDecimal").and_then(Value::as_u64).map(|d| d as u32);
    let mut out = HashMap::new();
    for l in lines {
        let (Some(id), Some(p)) = (l.get("offerId").and_then(Value::as_str), l.get("price")) else { continue };
        let Some(cur) = p.get("currencyCode").and_then(Value::as_str) else { continue };
        let (Some(orig), Some(cur_price)) = (p.get("originalPrice").and_then(Value::as_i64), p.get("discountPrice").and_then(Value::as_i64)) else { continue };
        out.insert(id.to_string(), (epic_money(orig, cur, pay_dec), epic_money(cur_price, cur, pay_dec)));
    }
    Ok(out)
}

/// `GET .../catalog/api/shared/bulk/offers` -> offer id -> (original, current), plus the namespace.
/// Regular price, price now, namespace.
pub type CatalogOffer = (Money, Money, Option<String>);

pub fn parse_catalog_bulk(text: &str) -> Result<HashMap<String, CatalogOffer>> {
    let v: Value = serde_json::from_str(text)?;
    let m = v.as_object().ok_or_else(|| Error::Parse("catalog: not an object".into()))?;
    let mut out = HashMap::new();
    for (id, o) in m {
        let Some(cur) = o.get("currencyCode").and_then(Value::as_str) else { continue };
        let dec = o.get("currencyDecimals").and_then(Value::as_u64).map(|d| d as u32);
        let now = o.get("currentPrice").and_then(Value::as_i64);
        let base = o.get("basePrice").or_else(|| o.get("price")).and_then(Value::as_i64);
        let (Some(now), Some(base)) = (now, base) else { continue };
        out.insert(id.clone(), (epic_money(base.max(now), cur, dec), epic_money(now, cur, dec), o.get("namespace").and_then(Value::as_str).map(str::to_string)));
    }
    Ok(out)
}

/// The account's billing country and currency (`.../billingaccounts/default`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Billing {
    pub country: String,
    pub currency: String,
}

pub fn parse_billing(text: &str) -> Result<Billing> {
    let v: Value = serde_json::from_str(text)?;
    let country = v.get("country").and_then(Value::as_str).filter(|c| c.len() == 2).ok_or_else(|| Error::Parse("billing: no country".into()))?;
    Ok(Billing { country: country.to_ascii_uppercase(), currency: v.get("currency").and_then(Value::as_str).unwrap_or("").to_string() })
}

// ------------------------------------------------------------------------------------------------ source

pub struct EpicAccount {
    net: Arc<Net>,
    db: StoreDb,
    provider: Arc<dyn EpicSessionProvider>,
    guard: std::sync::Mutex<Guard>,
}

/// Keeps a refusing Epic from turning into a refresh storm: Epic rotates refresh tokens, so refreshing again and
/// again because a service says no would be hammering the account for nothing.
#[derive(Default)]
struct Guard {
    /// When the login was last refreshed because a service refused the token.
    last_refresh: Option<std::time::Instant>,
    /// Routes (`graphql`, `price`, `catalog`, `billing`) that refused us, and until when they are left alone.
    blocked: HashMap<&'static str, std::time::Instant>,
}

/// A forced refresh at most this often.
const REFRESH_EVERY: std::time::Duration = std::time::Duration::from_secs(600);
/// A route that refused even a fresh token is left alone this long.
const BLOCK_FOR: std::time::Duration = std::time::Duration::from_secs(3600);

enum Call {
    Ok(crate::net::Reply),
    /// The route refuses us (401 after a fresh token, or 403), or is being left alone.
    Refused,
}

impl EpicAccount {
    pub fn new(net: Arc<Net>, db: StoreDb, provider: Arc<dyn EpicSessionProvider>) -> EpicAccount {
        EpicAccount { net, db, provider, guard: std::sync::Mutex::new(Guard::default()) }
    }

    fn blocked(&self, route: &'static str) -> bool {
        let g = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        g.blocked.get(route).is_some_and(|until| *until > std::time::Instant::now())
    }

    fn block(&self, route: &'static str) {
        self.guard.lock().unwrap_or_else(|e| e.into_inner()).blocked.insert(route, std::time::Instant::now() + BLOCK_FOR);
        tracing::debug!("store: Epic's {route} refuses this login; leaving it alone for an hour");
    }

    /// One request with the bearer token. A 401 refreshes the session (through the app's login, which handles
    /// Epic's token rotation) and repeats the request once, and not more than once every ten minutes; a route
    /// that still refuses, or says 403, is left alone for an hour.
    async fn authed(&self, route: &'static str, session: &mut EpicSession, method: &str, url: &str, body: Option<&Value>) -> Result<Call> {
        if self.blocked(route) {
            return Ok(Call::Refused);
        }
        for attempt in 0..2 {
            let bearer = format!("Bearer {}", session.token.expose());
            let headers = [("Authorization", bearer.as_str()), ("User-Agent", LAUNCHER_UA)];
            let r = match (method, body) {
                ("POST", Some(b)) => self.net.post_json(Svc::Light, url, &headers, b).await?,
                _ => self.net.get(Svc::Light, url, &headers).await?,
            };
            if r.status == 403 {
                self.block(route);
                return Ok(Call::Refused);
            }
            if r.status == 401 {
                let may_refresh = attempt == 0 && {
                    let mut g = self.guard.lock().unwrap_or_else(|e| e.into_inner());
                    let ok = g.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
                    if ok {
                        g.last_refresh = Some(std::time::Instant::now());
                    }
                    ok
                };
                if may_refresh {
                    *session = self.provider.refresh().await?;
                    continue;
                }
                self.block(route);
                return Ok(Call::Refused);
            }
            return Ok(Call::Ok(r));
        }
        Ok(Call::Refused)
    }

    fn slug_url(slug: &str) -> String {
        format!("https://store.epicgames.com/en-US/p/{slug}")
    }

    /// Where the product page of an offer is, from what grapple has stored.
    fn url_for(&self, namespace: &str, offer: &str) -> String {
        let key = format!("epic:{namespace}:{offer}");
        if let Ok(Some((json, _))) = self.db.epic_offer(&key)
            && let Ok(o) = serde_json::from_str::<EpicOffer>(&json)
        {
            return o.url();
        }
        if let Ok(Some(slug)) = self.db.epic_map_slug(namespace, offer) {
            return Self::slug_url(&slug);
        }
        "https://store.epicgames.com/".to_string()
    }

    /// Epic offers for a title (the launcher's store search), with their prices in `country`.
    pub async fn search(&self, session: &mut EpicSession, title: &str, country: &str) -> Result<Vec<EpicOffer>> {
        let url = format!("{}/graphql", self.net.endpoints.epic_graphql);
        let cc = country.to_ascii_uppercase();
        let body = json!({
            "query": SEARCH_QUERY,
            "variables": {"allowCountries": cc, "category": "games/edition/base|bundles/games|editors", "count": 10,
                           "country": cc, "keywords": title, "locale": "en-US", "sortBy": "relevancy", "sortDir": "DESC"}
        });
        match self.authed("graphql", session, "POST", &url, Some(&body)).await? {
            Call::Ok(r) if r.status == 200 => parse_free_games(&r.text()),
            Call::Ok(r) => Err(Error::Status(r.status)),
            Call::Refused => Err(Error::Unavailable("Epic refused the login".into())),
        }
    }

    /// The price engine for one namespace's offers.
    async fn price_engine(&self, session: &mut EpicSession, country: &str, offers: &[&str]) -> Result<HashMap<String, (Money, Money)>> {
        let url = format!("{}/priceengine/api/shared/offers/price", self.net.endpoints.epic_price);
        let body = json!({
            "accountId": session.account_id, "calculateTax": false, "country": country.to_ascii_uppercase(),
            "lineOffers": offers.iter().map(|o| json!({"offerId": o, "quantity": 1})).collect::<Vec<_>>()
        });
        match self.authed("price", session, "POST", &url, Some(&body)).await? {
            Call::Ok(r) if r.status == 200 => parse_price_engine(&r.text()),
            Call::Ok(r) => Err(Error::Status(r.status)),
            Call::Refused => Err(Error::Unavailable("Epic refused the login".into())),
        }
    }

    async fn catalog_bulk(&self, session: &mut EpicSession, country: &str, offers: &[&str]) -> Result<HashMap<String, CatalogOffer>> {
        let ids: String = offers.iter().map(|o| format!("id={o}")).collect::<Vec<_>>().join("&");
        let url = format!("{}/catalog/api/shared/bulk/offers?{ids}&returnItemDetails=false&country={}&locale=en", self.net.endpoints.epic_catalog, country.to_ascii_uppercase());
        match self.authed("catalog", session, "GET", &url, None).await? {
            Call::Ok(r) if r.status == 200 => parse_catalog_bulk(&r.text()),
            Call::Ok(r) => Err(Error::Status(r.status)),
            Call::Refused => Err(Error::Unavailable("Epic refused the login".into())),
        }
    }

    /// The account's billing country/currency (cached a day in `kv`): what Epic will charge him in.
    pub async fn billing(&self) -> Result<Billing> {
        if let Ok(Some((b, at))) = self.db.kv_get::<Billing>("epic_billing")
            && crate::db::now() - at < 86_400
        {
            return Ok(b);
        }
        let mut session = self.provider.session().await?;
        let url = format!("{}/launcher/api/public/payment/accounts/{}/billingaccounts/default", self.net.endpoints.epic_launcher, session.account_id);
        match self.authed("billing", &mut session, "GET", &url, None).await? {
            Call::Ok(r) if r.status == 200 => {
                let b = parse_billing(&r.text())?;
                self.db.kv_set("epic_billing", &b)?;
                Ok(b)
            }
            Call::Ok(r) => Err(Error::Status(r.status)),
            Call::Refused => Err(Error::Unavailable("Epic refused the login".into())),
        }
    }

    /// Prices of offers by id: the price engine per namespace, the catalog for what it did not answer.
    async fn prices_by_id(&self, country: &str, offers: &[OfferRef]) -> Result<HashMap<OfferRef, Option<EpicQuote>>> {
        let mut session = self.provider.session().await?;
        let mut out: HashMap<OfferRef, Option<EpicQuote>> = HashMap::new();
        let mut by_ns: Vec<(String, Vec<String>)> = Vec::new();
        for (ns, id) in offers {
            match by_ns.iter_mut().find(|(n, _)| n == ns) {
                Some((_, v)) => v.push(id.clone()),
                None => by_ns.push((ns.clone(), vec![id.clone()])),
            }
        }
        let mut engine_failed = false;
        for (ns, ids) in &by_ns {
            for chunk in ids.chunks(PRICE_CHUNK) {
                let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
                match self.price_engine(&mut session, country, &refs).await {
                    Ok(m) => {
                        for id in chunk {
                            if let Some((orig, now)) = m.get(id) {
                                out.insert((ns.clone(), id.clone()), Some(quote(ns, id, orig.clone(), now.clone(), self.url_for(ns, id))));
                            }
                        }
                    }
                    Err(Error::RateLimited) => return Err(Error::RateLimited),
                    Err(e) => {
                        tracing::debug!("store: epic price engine: {e}");
                        engine_failed = true;
                    }
                }
            }
        }
        // The catalog answers for whatever the price engine did not (or refused).
        let missing: Vec<&OfferRef> = offers.iter().filter(|o| !out.contains_key(*o)).collect();
        if !missing.is_empty() {
            let _ = engine_failed;
            for chunk in missing.chunks(PRICE_CHUNK) {
                let refs: Vec<&str> = chunk.iter().map(|(_, id)| id.as_str()).collect();
                match self.catalog_bulk(&mut session, country, &refs).await {
                    Ok(m) => {
                        for (ns, id) in chunk.iter().map(|o| (&o.0, &o.1)) {
                            if let Some((orig, now, _)) = m.get(id) {
                                out.insert((ns.clone(), id.clone()), Some(quote(ns, id, orig.clone(), now.clone(), self.url_for(ns, id))));
                            }
                        }
                    }
                    Err(Error::RateLimited) => return Err(Error::RateLimited),
                    Err(e) => tracing::debug!("store: epic catalog: {e}"),
                }
            }
        }
        if out.is_empty() && !offers.is_empty() {
            return Err(Error::Unavailable("Epic did not answer".into()));
        }
        Ok(out)
    }

    async fn quotes_for(&self, country: &str, games: &[SteamRef]) -> Result<HashMap<u32, Option<EpicQuote>>> {
        let ids: Vec<u32> = games.iter().map(|g| g.appid).collect();
        let maps = self.db.epic_maps(&ids)?;
        let now = crate::db::now();
        let mut out: HashMap<u32, Option<EpicQuote>> = HashMap::new();
        let mut mapped: Vec<(u32, OfferRef)> = Vec::new();
        let mut to_search: Vec<&SteamRef> = Vec::new();
        for g in games {
            match maps.get(&g.appid) {
                Some((Some(m), at)) if now - at < MAP_TTL => mapped.push((g.appid, (m.namespace.clone(), m.offer.clone()))),
                Some((None, at)) if now - at < NO_MAP_TTL => {
                    out.insert(g.appid, None);
                }
                _ => to_search.push(g),
            }
        }
        // First sight of a game: one store search finds the offer and prices it in the same answer.
        if !to_search.is_empty() {
            let mut session = self.provider.session().await?;
            for g in to_search {
                let offers = match self.search(&mut session, &g.name, country).await {
                    Ok(o) => o,
                    Err(Error::RateLimited) => return Err(Error::RateLimited),
                    Err(e @ Error::Unavailable(_)) => return Err(e),
                    Err(e) => {
                        tracing::debug!("store: epic search: {e}");
                        continue;
                    }
                };
                match resolve(g, &offers) {
                    Some(o) => {
                        self.db.put_epic_map(g.appid, Some((&o.namespace, &o.offer_id, o.page_slug.as_deref().unwrap_or(""), &o.title)))?;
                        let _ = self.db.put_epic_offer(&o.key(), &serde_json::to_string(&o)?);
                        let mut q = quote(&o.namespace, &o.offer_id, o.original.clone(), o.current.clone(), o.url());
                        q.steam_appid = g.appid;
                        out.insert(g.appid, Some(q));
                    }
                    None => {
                        self.db.put_epic_map(g.appid, None)?;
                        out.insert(g.appid, None);
                    }
                }
            }
        }
        // Known matches: current prices by offer id.
        if !mapped.is_empty() {
            let offers: Vec<OfferRef> = mapped.iter().map(|(_, o)| o.clone()).collect();
            let prices = self.prices_by_id(country, &offers).await?;
            for (appid, o) in mapped {
                if let Some(Some(q)) = prices.get(&o) {
                    let mut q = q.clone();
                    q.steam_appid = appid;
                    out.insert(appid, Some(q));
                }
            }
        }
        Ok(out)
    }
}

impl EpicPriceSource for EpicAccount {
    fn name(&self) -> &'static str {
        "epic-account"
    }

    fn available(&self) -> bool {
        self.provider.available()
    }

    fn quotes<'a>(&'a self, country: &'a str, games: &'a [SteamRef]) -> BoxFuture<'a, Result<HashMap<u32, Option<EpicQuote>>>> {
        Box::pin(self.quotes_for(country, games))
    }

    fn offer_prices<'a>(&'a self, country: &'a str, offers: &'a [OfferRef]) -> BoxFuture<'a, Result<HashMap<OfferRef, Option<EpicQuote>>>> {
        Box::pin(self.prices_by_id(country, offers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(title: &str, makers: &[&str], offer_type: &str) -> EpicOffer {
        EpicOffer {
            namespace: "ns".into(),
            offer_id: format!("{title}-{offer_type}"),
            title: title.into(),
            offer_type: offer_type.into(),
            page_slug: None,
            cover: None,
            wide: None,
            thumb: None,
            seller: None,
            makers: makers.iter().map(|s| s.to_string()).collect(),
            description: None,
            original: Money::new(100, "USD"),
            current: Money::new(100, "USD"),
            free_now: None,
            free_next: None,
        }
    }

    fn game(name: &str, devs: &[&str], pubs: &[&str]) -> SteamRef {
        SteamRef { appid: 1, name: name.into(), developers: devs.iter().map(|s| s.to_string()).collect(), publishers: pubs.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn titles_compare_without_case_or_marks_but_never_loosely() {
        assert_eq!(norm_title("Middle-earth™: Shadow of War™"), norm_title("Middle-Earth: Shadow Of War"));
        assert_eq!(norm_title("Dungeons & Dragons"), norm_title("Dungeons and Dragons"));
        assert_ne!(norm_title("Core Keeper"), norm_title("Core Keeper: Deluxe Edition"));
        assert_ne!(norm_title("Hades"), norm_title("Hades II"));
    }

    #[test]
    fn makers_match_by_company_not_by_word() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(same_maker(&s(&["CD PROJEKT RED"]), &s(&["CD PROJEKT RED"])));
        assert!(same_maker(&s(&["Pugstorm"]), &s(&["Fireshine Games", "Pugstorm"])));
        assert!(same_maker(&s(&["Acme Games, Inc."]), &s(&["acme games"])), "legal suffixes do not count");
        assert!(!same_maker(&s(&["Acme Games"]), &s(&["Acme Studios"])), "a shared word is not a shared company");
        assert!(!same_maker(&s(&[]), &s(&["Acme"])));
        assert!(!same_maker(&s(&["Inc."]), &s(&["LLC"])), "nothing left after dropping suffixes: no match");
    }

    #[test]
    fn a_game_is_matched_only_with_title_and_maker() {
        let core = game("Core Keeper", &["Pugstorm"], &["Fireshine Games"]);
        let ok = offer("Core Keeper", &["Pugstorm"], "BASE_GAME");
        assert_eq!(resolve(&core, &[ok.clone()]).unwrap().offer_id, ok.offer_id);
        // same title, different company: another game
        assert!(resolve(&core, &[offer("Core Keeper", &["Someone Else"], "BASE_GAME")]).is_none());
        // same company, different title (an edition, a sequel)
        assert!(resolve(&core, &[offer("Core Keeper Deluxe", &["Pugstorm"], "BASE_GAME"), offer("Core Keeper 2", &["Pugstorm"], "BASE_GAME")]).is_none());
        // two editions with the title: only a single base game settles it
        let base = offer("Core Keeper", &["Pugstorm"], "BASE_GAME");
        let ed = offer("Core Keeper", &["Pugstorm"], "EDITION");
        assert_eq!(resolve(&core, &[ed.clone(), base.clone()]).unwrap().offer_type, "BASE_GAME");
        assert!(resolve(&core, &[ed.clone(), offer("Core Keeper", &["Pugstorm"], "EDITION")]).is_none());
        assert!(resolve(&core, &[]).is_none());
        assert!(resolve(&game("", &["x"], &[]), &[base]).is_none());
    }

    #[test]
    fn price_engine_answer_gives_prices_and_computes_the_discount() {
        let text = json!({"country": "ZA", "namespace": "ns", "totalPaymentPrice": {"paymentCurrencyCode": "USD", "paymentCurrencyDecimal": 2},
            "lineOffers": [{"offerId": "o1", "price": {"currencyCode": "USD", "originalPrice": 1999, "discountPrice": 999, "discountPercentage": 100}},
                           {"offerId": "o2", "price": {"currencyCode": "USD", "originalPrice": 500, "discountPrice": 500, "discountPercentage": 100}},
                           {"offerId": "broken"}]})
        .to_string();
        let m = parse_price_engine(&text).unwrap();
        assert_eq!(m["o1"], (Money::new(1999, "USD"), Money::new(999, "USD")));
        let q = quote("ns", "o1", m["o1"].0.clone(), m["o1"].1.clone(), "u".into());
        assert_eq!(q.discount, 50, "worked out from the prices, not from discountPercentage");
        assert_eq!(quote("ns", "o2", m["o2"].0.clone(), m["o2"].1.clone(), "u".into()).discount, 0);
        assert!(!m.contains_key("broken"));
        assert!(parse_price_engine("{}").is_err());
    }

    #[test]
    fn catalog_and_billing_shapes() {
        let text = json!({"o1": {"title": "X", "namespace": "ns", "currencyCode": "USD", "currencyDecimals": 2, "currentPrice": 999, "price": 1999, "basePrice": 1999}}).to_string();
        let m = parse_catalog_bulk(&text).unwrap();
        assert_eq!((m["o1"].0.minor, m["o1"].1.minor, m["o1"].2.as_deref()), (1999, 999, Some("ns")));
        assert_eq!(parse_billing("{\"billingAccountName\":\"x\",\"country\":\"za\",\"countrySource\":\"a\",\"currency\":\"USD\"}").unwrap(), Billing { country: "ZA".into(), currency: "USD".into() });
        assert!(parse_billing("{}").is_err());
    }

    #[test]
    fn the_session_never_prints_its_token() {
        let s = EpicSession { account_id: "acc".into(), token: ApiKey::new("tok-secret") };
        assert!(!format!("{s:?}").contains("tok-secret"));
    }
}
