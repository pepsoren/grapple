//! Epic: the public free-games feed (real prices for the offers in it) and the pluggable source for Epic prices
//! of everything else.
//!
//! Epic's catalog GraphQL is closed to outside callers (403), so Epic prices for other games are **not
//! invented**: they come from an [`EpicPriceSource`]. Today there are two: [`Unavailable`] (says so) and
//! [`Itad`] (IsThereAnyDeal, with a key Josh provides, kept in the keyring). A source for Epic's authenticated
//! price service can be added next to them without touching anything else.

use crate::db::StoreDb;
use crate::error::{Error, Result};
use crate::money::{Money, decimals, from_major};
use crate::net::{Net, Svc};
use crate::secrets::{ApiKey, KeyStore};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// When a promotion runs (ISO 8601 as Epic gives it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from: String,
    pub until: String,
}

/// An offer from the free-games feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicOffer {
    pub namespace: String,
    pub offer_id: String,
    pub title: String,
    pub offer_type: String,
    pub page_slug: Option<String>,
    pub cover: Option<String>,
    pub wide: Option<String>,
    pub thumb: Option<String>,
    pub seller: Option<String>,
    /// Seller, developer and publisher names as the offer lists them (for matching a game across stores).
    #[serde(default)]
    pub makers: Vec<String>,
    pub description: Option<String>,
    /// Regular price and the price right now, in the feed's country currency.
    pub original: Money,
    pub current: Money,
    /// Free right now (until `until`).
    pub free_now: Option<Window>,
    /// Free from a coming date.
    pub free_next: Option<Window>,
}

impl EpicOffer {
    pub fn key(&self) -> String {
        format!("epic:{}:{}", self.namespace, self.offer_id)
    }

    pub fn url(&self) -> String {
        match &self.page_slug {
            Some(s) => format!("https://store.epicgames.com/en-US/p/{s}"),
            None => format!("https://store.epicgames.com/en-US/browse?q={}", self.title.replace(' ', "%20")),
        }
    }

    pub fn discount_pct(&self) -> u32 {
        if self.original.minor <= 0 || self.current.minor >= self.original.minor {
            return 0;
        }
        (100.0 - 100.0 * self.current.minor as f64 / self.original.minor as f64).round() as u32
    }
}

fn money(v: &Value, key: &str, currency: &str, dec: u32) -> Money {
    let raw = v.get(key).and_then(Value::as_i64).unwrap_or(0);
    // The feed counts in its own decimals; Money counts in ISO ones.
    Money::new(from_major(raw as f64 / 10f64.powi(dec as i32), currency), currency)
}

fn windows(promos: Option<&Value>, free_only: bool) -> Vec<Window> {
    let Some(groups) = promos.and_then(Value::as_array) else { return Vec::new() };
    groups
        .iter()
        .filter_map(|g| g.get("promotionalOffers")?.as_array())
        .flatten()
        .filter(|o| {
            // Epic's "discountPercentage" is what the buyer still pays: 0 means free.
            !free_only || o.pointer("/discountSetting/discountPercentage").and_then(Value::as_i64) == Some(0)
        })
        .filter_map(|o| Some(Window { from: o.get("startDate")?.as_str()?.to_string(), until: o.get("endDate")?.as_str()?.to_string() }))
        .collect()
}

/// `freeGamesPromotions`: every offer in the feed with its real price in the feed's country currency.
pub fn parse_free_games(text: &str) -> Result<Vec<EpicOffer>> {
    let v: Value = serde_json::from_str(text)?;
    let els = v.pointer("/data/Catalog/searchStore/elements").and_then(Value::as_array).ok_or_else(|| Error::Parse("epic: no elements".into()))?;
    let mut out = Vec::new();
    for e in els {
        let (Some(ns), Some(id), Some(title)) = (e.get("namespace").and_then(Value::as_str), e.get("id").and_then(Value::as_str), e.get("title").and_then(Value::as_str)) else { continue };
        let offer_type = e.get("offerType").and_then(Value::as_str).unwrap_or("").to_string();
        if !matches!(offer_type.as_str(), "BASE_GAME" | "EDITION") || e.get("isCodeRedemptionOnly").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let tp = e.pointer("/price/totalPrice");
        let currency = tp.and_then(|t| t.get("currencyCode")).and_then(Value::as_str).unwrap_or("USD");
        let dec = tp.and_then(|t| t.pointer("/currencyInfo/decimals")).and_then(Value::as_u64).map_or_else(|| decimals(currency), |d| d as u32);
        let (original, current) = match tp {
            Some(t) => (money(t, "originalPrice", currency, dec), money(t, "discountPrice", currency, dec)),
            None => (Money::new(0, currency), Money::new(0, currency)),
        };
        let img = |kind: &str| -> Option<String> {
            e.get("keyImages")?.as_array()?.iter().find(|i| i.get("type").and_then(Value::as_str) == Some(kind)).and_then(|i| i.get("url")?.as_str().map(str::to_string))
        };
        let page_slug = e
            .get("offerMappings")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find(|m| m.get("pageType").and_then(Value::as_str) == Some("productHome")))
            .and_then(|m| m.get("pageSlug")?.as_str().map(str::to_string))
            .or_else(|| e.get("productSlug").and_then(Value::as_str).and_then(|s| s.split('/').next()).filter(|s| !s.is_empty()).map(str::to_string));
        let promos = e.get("promotions").filter(|p| !p.is_null());
        out.push(EpicOffer {
            namespace: ns.to_string(),
            offer_id: id.to_string(),
            title: title.to_string(),
            offer_type,
            page_slug,
            cover: img("OfferImageTall"),
            wide: img("OfferImageWide"),
            thumb: img("Thumbnail"),
            seller: e.pointer("/seller/name").and_then(Value::as_str).map(str::to_string),
            makers: {
                let mut m: Vec<String> = e.pointer("/seller/name").and_then(Value::as_str).map(str::to_string).into_iter().collect();
                if let Some(attrs) = e.get("customAttributes").and_then(Value::as_array) {
                    for a in attrs {
                        if matches!(a.get("key").and_then(Value::as_str), Some("developerName" | "publisherName"))
                            && let Some(v) = a.get("value").and_then(Value::as_str).filter(|v| !v.is_empty())
                            && !m.iter().any(|x| x == v)
                        {
                            m.push(v.to_string());
                        }
                    }
                }
                m
            },
            description: e.get("description").and_then(Value::as_str).filter(|d| !d.is_empty() && *d != title).map(|d| d.chars().take(300).collect()),
            original,
            current,
            free_now: windows(promos.and_then(|p| p.get("promotionalOffers")), true).into_iter().next(),
            free_next: windows(promos.and_then(|p| p.get("upcomingPromotionalOffers")), true).into_iter().next(),
        });
    }
    Ok(out)
}

pub struct EpicFree {
    net: Arc<Net>,
}

impl EpicFree {
    pub fn new(net: Arc<Net>) -> EpicFree {
        EpicFree { net }
    }

    pub async fn fetch(&self, country: &str) -> Result<Vec<EpicOffer>> {
        let cc: String = country.chars().filter(char::is_ascii_alphabetic).take(2).collect::<String>().to_ascii_uppercase();
        let url = format!("{}?locale=en-US&country={cc}&allowCountries={cc}", self.net.endpoints.epic_free);
        let r = self.net.get(Svc::Light, &url, &[]).await?;
        if r.status != 200 {
            return Err(Error::Status(r.status));
        }
        parse_free_games(&r.text())
    }
}

// ---------------------------------------------------------------------------------------------------- prices

/// Epic's price for a game that Steam also sells (or for an Epic offer), as a source reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicQuote {
    /// The Steam app this was asked for; 0 for a price asked by offer id only.
    pub steam_appid: u32,
    /// Regular price and price now, in the currency of the country asked for.
    pub original: Money,
    pub price: Money,
    pub discount: u32,
    /// Where Buy goes (the Epic product page, or the source's own link that lands on it).
    pub url: String,
    /// The lowest price any store has had, when the source knows it.
    pub history_low: Option<Money>,
    /// Epic's ids, when the source knows them: Buy can then open the real checkout.
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub offer_id: Option<String>,
}

/// A Steam game as a source needs it to find the same game on Epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamRef {
    pub appid: u32,
    pub name: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
}

/// An Epic offer: catalog namespace and offer id.
pub type OfferRef = (String, String);

/// Where Epic prices come from.
///
/// Two ways in: by Steam game (`quotes`), for games Epic may also sell, and by Epic offer id (`offer_prices`), for
/// offers grapple already has the ids of (the free-games feed, wishlisted Epic games, games matched earlier).
pub trait EpicPriceSource: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whether the source can answer now (a key is set, a login exists...). Cheap, no network.
    fn available(&self) -> bool;
    /// `Some(quote)`: Epic sells it at that price. `None`: looked up, no Epic price known for it. Games missing
    /// from the map were not answered. A source never guesses: it answers `Some` only when sure it is the same game.
    fn quotes<'a>(&'a self, country: &'a str, games: &'a [SteamRef]) -> BoxFuture<'a, Result<HashMap<u32, Option<EpicQuote>>>>;
    /// Prices of Epic offers by their ids. Offers missing from the map were not answered.
    fn offer_prices<'a>(&'a self, _country: &'a str, _offers: &'a [OfferRef]) -> BoxFuture<'a, Result<HashMap<OfferRef, Option<EpicQuote>>>> {
        Box::pin(async { Err(Error::Unavailable(format!("{} cannot price Epic offers by id", self.name()))) })
    }
}

/// No source: Epic prices show as unavailable, never as a guess.
pub struct Unavailable;

impl EpicPriceSource for Unavailable {
    fn name(&self) -> &'static str {
        "none"
    }
    fn available(&self) -> bool {
        false
    }
    fn quotes<'a>(&'a self, _country: &'a str, _games: &'a [SteamRef]) -> BoxFuture<'a, Result<HashMap<u32, Option<EpicQuote>>>> {
        Box::pin(async { Err(Error::Unavailable("no Epic price source".into())) })
    }
}

/// IsThereAnyDeal (<https://docs.isthereanydeal.com>): game ids from Steam app ids, then Epic's price (shop 16).
/// The key goes in the `ITAD-API-Key` header, so it is never in a URL.
pub struct Itad {
    net: Arc<Net>,
    keys: Arc<dyn KeyStore>,
    db: StoreDb,
    has_key: AtomicBool,
}

const ITAD_STEAM_SHOP: u32 = 61;
const ITAD_EPIC_SHOP: u32 = 16;

impl Itad {
    pub fn new(net: Arc<Net>, keys: Arc<dyn KeyStore>, db: StoreDb) -> Itad {
        Itad { net, keys, db, has_key: AtomicBool::new(false) }
    }

    /// Read the key store once (blocking: D-Bus).
    pub fn refresh_key(&self) {
        let has = matches!(self.keys.load(), Ok(Some(_)));
        self.has_key.store(has, Ordering::Relaxed);
    }

    pub fn set_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid("that does not look like an API key".into()));
        }
        self.keys.save(&ApiKey::new(key))?;
        self.has_key.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn clear_key(&self) -> Result<()> {
        self.keys.clear()?;
        self.has_key.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn key(&self) -> Result<ApiKey> {
        self.keys.load()?.ok_or_else(|| Error::Unavailable("no IsThereAnyDeal key".into()))
    }

    async fn gids(&self, key: &ApiKey, ids: &[u32]) -> Result<HashMap<u32, Option<String>>> {
        let mut known = self.db.itad_gids(ids)?;
        let missing: Vec<u32> = ids.iter().copied().filter(|i| !known.contains_key(i)).collect();
        if !missing.is_empty() {
            let url = format!("{}/lookup/id/shop/{ITAD_STEAM_SHOP}/v1", self.net.endpoints.itad);
            let body = Value::Array(missing.iter().map(|i| json!(format!("app/{i}"))).collect());
            let r = self.net.post_json(Svc::Light, &url, &[("ITAD-API-Key", key.expose())], &body).await?;
            if r.status == 401 || r.status == 403 {
                return Err(Error::Unavailable("IsThereAnyDeal refused the key".into()));
            }
            if r.status != 200 {
                return Err(Error::Status(r.status));
            }
            let map: Value = r.json()?;
            for id in missing {
                let gid = map.get(format!("app/{id}")).and_then(Value::as_str).map(str::to_string);
                self.db.put_itad_gid(id, gid.as_deref())?;
                known.insert(id, gid);
            }
        }
        Ok(known)
    }
}

fn itad_money(v: &Value) -> Option<Money> {
    let cur = v.get("currency")?.as_str()?;
    // `amountInt` is in the currency's minor unit.
    Some(Money::new(v.get("amountInt").and_then(Value::as_i64).unwrap_or_else(|| from_major(v.get("amount").and_then(Value::as_f64).unwrap_or(0.0), cur)), cur))
}

/// `games/prices/v3` -> the Epic deal of each game id.
/// An Epic deal from ITAD: regular price, price now, cut, link, all-time low.
pub type ItadDeal = (Money, Money, u32, String, Option<Money>);

pub fn parse_itad_prices(text: &str) -> Result<HashMap<String, Option<ItadDeal>>> {
    let v: Value = serde_json::from_str(text)?;
    let arr = v.as_array().ok_or_else(|| Error::Parse("itad: prices is not a list".into()))?;
    let mut out = HashMap::new();
    for g in arr {
        let Some(id) = g.get("id").and_then(Value::as_str) else { continue };
        let deal = g.get("deals").and_then(Value::as_array).and_then(|d| d.iter().find(|x| x.pointer("/shop/id").and_then(Value::as_u64) == Some(ITAD_EPIC_SHOP as u64)));
        let entry = deal.and_then(|d| {
            Some((
                itad_money(d.get("regular")?)?,
                itad_money(d.get("price")?)?,
                d.get("cut").and_then(Value::as_u64).unwrap_or(0) as u32,
                d.get("url")?.as_str()?.to_string(),
                g.pointer("/historyLow/all").and_then(itad_money),
            ))
        });
        out.insert(id.to_string(), entry);
    }
    Ok(out)
}

impl EpicPriceSource for Itad {
    fn name(&self) -> &'static str {
        "isthereanydeal"
    }

    fn available(&self) -> bool {
        self.has_key.load(Ordering::Relaxed)
    }

    fn quotes<'a>(&'a self, country: &'a str, games: &'a [SteamRef]) -> BoxFuture<'a, Result<HashMap<u32, Option<EpicQuote>>>> {
        Box::pin(async move {
            let ids: Vec<u32> = games.iter().map(|g| g.appid).collect();
            let key = self.key()?;
            let cc: String = country.chars().filter(char::is_ascii_alphabetic).take(2).collect::<String>().to_ascii_uppercase();
            let mut out: HashMap<u32, Option<EpicQuote>> = HashMap::new();
            for chunk in ids.chunks(100) {
                let gids = self.gids(&key, chunk).await?;
                let mut by_gid: HashMap<String, u32> = HashMap::new();
                for (&id, g) in &gids {
                    match g {
                        Some(g) => {
                            by_gid.insert(g.clone(), id);
                        }
                        None => {
                            out.insert(id, None); // ITAD does not know the game: not on Epic as far as it can tell
                        }
                    }
                }
                if by_gid.is_empty() {
                    continue;
                }
                let url = format!("{}/games/prices/v3?country={cc}&shops={ITAD_EPIC_SHOP}&deals=false&vouchers=false", self.net.endpoints.itad);
                let body = Value::Array(by_gid.keys().map(|g| json!(g)).collect());
                let r = self.net.post_json(Svc::Light, &url, &[("ITAD-API-Key", key.expose())], &body).await?;
                if r.status == 401 || r.status == 403 {
                    return Err(Error::Unavailable("IsThereAnyDeal refused the key".into()));
                }
                if r.status != 200 {
                    return Err(Error::Status(r.status));
                }
                let prices = parse_itad_prices(&r.text())?;
                for (gid, id) in by_gid {
                    match prices.get(&gid) {
                        Some(Some((orig, price, cut, url, low))) => {
                            out.insert(id, Some(EpicQuote { steam_appid: id, original: orig.clone(), price: price.clone(), discount: *cut, url: url.clone(), history_low: low.clone(), namespace: None, offer_id: None }));
                        }
                        Some(None) | None => {
                            out.insert(id, None);
                        }
                    }
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn free_feed_gives_real_prices_and_windows() {
        let offers = parse_free_games(&fx("epic_free.json")).unwrap();
        let bad = offers.iter().find(|o| o.title == "Bad Cheese").unwrap();
        assert_eq!(bad.key(), "epic:a0c80bf673a548bc97b4bb25abfaf2b4:6a846880836044a2adad614ac771f487");
        assert_eq!((bad.original.minor, bad.current.minor, bad.original.currency.as_str()), (9969, 0, "ZAR"));
        let w = bad.free_now.as_ref().unwrap();
        assert_eq!(w.until, "2026-10-22T15:00:00.000Z");
        assert!(bad.free_next.is_none());
        assert_eq!(bad.discount_pct(), 100);
        assert_eq!(bad.url(), "https://store.epicgames.com/en-US/p/bad-cheese-4256d9");
        assert!(bad.cover.as_deref().unwrap().starts_with("https://"));

        let sker = offers.iter().find(|o| o.title == "Sker Ritual").unwrap();
        assert!(sker.free_now.is_none() && sker.free_next.is_some(), "free from a coming date");
        // "Eternal Threads" has an upcoming 20%-of-price promotion: a sale, not a free game
        let et = offers.iter().find(|o| o.title == "Eternal Threads").unwrap();
        assert!(et.free_now.is_none() && et.free_next.is_none());
        let herds = offers.iter().find(|o| o.title.starts_with("Them")).unwrap();
        assert_eq!(herds.discount_pct(), 0);
    }

    #[test]
    fn feed_without_elements_is_an_error() {
        assert!(parse_free_games("{\"data\":{}}").is_err());
    }

    #[test]
    fn itad_prices_pick_the_epic_deal() {
        let text = json!([
            {"id": "g1", "historyLow": {"all": {"amount": 4.5, "amountInt": 450, "currency": "ZAR"}, "y1": null, "m3": null},
             "deals": [
                {"shop": {"id": 61, "name": "Steam"}, "price": {"amount": 99.0, "amountInt": 9900, "currency": "ZAR"}, "regular": {"amount": 199.0, "amountInt": 19900, "currency": "ZAR"}, "cut": 50, "url": "https://itad/steam"},
                {"shop": {"id": 16, "name": "Epic Game Store"}, "price": {"amount": 89.0, "amountInt": 8900, "currency": "ZAR"}, "regular": {"amount": 199.0, "amountInt": 19900, "currency": "ZAR"}, "cut": 55, "url": "https://itad/epic"}
             ]},
            {"id": "g2", "historyLow": {"all": null, "y1": null, "m3": null}, "deals": []}
        ])
        .to_string();
        let p = parse_itad_prices(&text).unwrap();
        let (orig, price, cut, url, low) = p["g1"].clone().unwrap();
        assert_eq!((orig.minor, price.minor, cut, url.as_str()), (19900, 8900, 55, "https://itad/epic"));
        assert_eq!(low, Some(Money::new(450, "ZAR")));
        assert!(p["g2"].is_none(), "no Epic deal: not on Epic");
    }

    #[tokio::test]
    async fn unavailable_says_so() {
        let u = Unavailable;
        assert!(!u.available());
        assert!(u.quotes("ZA", &[SteamRef { appid: 1, name: "x".into(), developers: vec![], publishers: vec![] }]).await.is_err());
        assert!(u.offer_prices("ZA", &[("a".into(), "b".into())]).await.is_err());
    }
}
