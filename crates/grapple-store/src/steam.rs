//! Steam's public store endpoints: lists, specials, details, bulk prices, community tags, wishlist, charts.
//! Parsers are plain functions on text (tested against recorded answers); [`Steam`] adds the requests.

use crate::error::{Error, Result};
use crate::net::{AGE_GATE_COOKIE, Net, Svc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Steam's wishlist account: 76561197960265728 + the account id (the folder in `userdata/`).
pub const STEAM_ID_BASE: u64 = 76_561_197_960_265_728;

pub fn steam_id64(account_id: u32) -> u64 {
    STEAM_ID_BASE + account_id as u64
}

/// A price as Steam reports it: hundredths of the major unit (see `money::from_steam`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamPrice {
    pub currency: String,
    pub initial: i64,
    pub r#final: i64,
    pub discount: u32,
}

/// A game in a list (search result or one of the featured lists).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListItem {
    pub appid: u32,
    pub name: String,
    /// Only the featured lists carry prices; search results do not.
    pub price: Option<SteamPrice>,
    /// When a discount ends, unix seconds (specials only).
    pub discount_expires: Option<i64>,
}

/// What `appdetails` says about a game, reduced to what the store shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub appid: u32,
    pub name: String,
    /// `game`, `dlc`, `demo`, `music`, `advertising`, ...
    pub kind: String,
    pub is_free: bool,
    pub short: String,
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    pub release: Option<String>,
    pub coming_soon: bool,
    pub genres: Vec<String>,
    /// Steam's content descriptor ids (1 and 3 and 4: sexual/nudity, 2: violence/gore, 5: general mature).
    pub descriptors: Vec<u32>,
    pub reviews: Option<u64>,
    /// Full-size screenshot URLs, in Steam's order.
    pub shots: Vec<String>,
    pub price: Option<SteamPrice>,
    pub windows: bool,
    pub linux: bool,
}

impl AppInfo {
    pub fn is_game(&self) -> bool {
        self.kind == "game"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// Steam's tag id (what `search/results?tags=` takes); 0 when unknown.
    #[serde(default)]
    pub id: u32,
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Featured {
    pub specials: Vec<ListItem>,
    pub top_sellers: Vec<ListItem>,
    pub new_releases: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WishItem {
    pub appid: u32,
    pub added: i64,
}

/// A bulk price answer for one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceLookup {
    Priced(SteamPrice),
    /// Steam answered, but there is no price (free to play, not out yet, not sold in this country).
    NoPrice,
}

fn app_from_url(url: &str) -> Option<u32> {
    let rest = url.split("/steam/apps/").nth(1)?;
    rest.split('/').next()?.parse().ok()
}

/// `search/results?json=1`: apps in order (packages and bundles are skipped).
pub fn parse_search(text: &str) -> Result<Vec<ListItem>> {
    let v: Value = serde_json::from_str(text)?;
    let items = v.get("items").and_then(Value::as_array).ok_or_else(|| Error::Parse("search: no items".into()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        let (Some(name), Some(logo)) = (it.get("name").and_then(Value::as_str), it.get("logo").and_then(Value::as_str)) else { continue };
        let Some(appid) = app_from_url(logo) else { continue };
        if seen.insert(appid) {
            out.push(ListItem { appid, name: name.to_string(), price: None, discount_expires: None });
        }
    }
    Ok(out)
}

fn price_from(v: &Value) -> Option<SteamPrice> {
    Some(SteamPrice {
        currency: v.get("currency")?.as_str()?.to_string(),
        initial: v.get("initial").or_else(|| v.get("original_price"))?.as_i64()?,
        r#final: v.get("final").or_else(|| v.get("final_price"))?.as_i64()?,
        discount: v.get("discount_percent").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

fn featured_list(v: &Value, key: &str) -> Vec<ListItem> {
    let Some(items) = v.pointer(&format!("/{key}/items")).and_then(Value::as_array) else { return Vec::new() };
    items
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_i64).unwrap_or(0) == 0) // 0 = app
        .filter_map(|i| {
            Some(ListItem {
                appid: u32::try_from(i.get("id")?.as_u64()?).ok()?,
                name: i.get("name")?.as_str()?.to_string(),
                price: price_from(i),
                discount_expires: i.get("discount_expiration").and_then(Value::as_i64),
            })
        })
        .collect()
}

/// `api/featuredcategories`.
pub fn parse_featured(text: &str) -> Result<Featured> {
    let v: Value = serde_json::from_str(text)?;
    if v.get("specials").is_none() && v.get("top_sellers").is_none() {
        return Err(Error::Parse("featuredcategories: unexpected shape".into()));
    }
    Ok(Featured { specials: featured_list(&v, "specials"), top_sellers: featured_list(&v, "top_sellers"), new_releases: featured_list(&v, "new_releases") })
}

/// `appdetails?filters=price_overview` for many ids.
pub fn parse_prices(text: &str) -> Result<HashMap<u32, PriceLookup>> {
    let v: Value = serde_json::from_str(text)?;
    let m = v.as_object().ok_or_else(|| Error::Parse("prices: not an object".into()))?;
    let mut out = HashMap::new();
    for (k, e) in m {
        let Ok(id) = k.parse::<u32>() else { continue };
        if e.get("success").and_then(Value::as_bool) != Some(true) {
            continue; // a failure for one id says nothing about its price
        }
        match e.pointer("/data/price_overview").and_then(price_from) {
            Some(p) => out.insert(id, PriceLookup::Priced(p)),
            None => out.insert(id, PriceLookup::NoPrice),
        };
    }
    Ok(out)
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ")
}

/// `appdetails?appids=<one id>`: `Ok(None)` when Steam has nothing for the id (removed, region-locked).
pub fn parse_app_details(text: &str, appid: u32) -> Result<Option<AppInfo>> {
    let v: Value = serde_json::from_str(text)?;
    let e = v.get(appid.to_string()).ok_or_else(|| Error::Parse("appdetails: id missing".into()))?;
    if e.get("success").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let d = e.get("data").ok_or_else(|| Error::Parse("appdetails: no data".into()))?;
    let strs = |p: &str| -> Vec<String> { d.pointer(p).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    Ok(Some(AppInfo {
        appid,
        name: unescape(d.get("name").and_then(Value::as_str).unwrap_or("")),
        kind: d.get("type").and_then(Value::as_str).unwrap_or("game").to_string(),
        is_free: d.get("is_free").and_then(Value::as_bool).unwrap_or(false),
        short: unescape(d.get("short_description").and_then(Value::as_str).unwrap_or("")),
        developers: strs("/developers"),
        publishers: strs("/publishers"),
        release: d.pointer("/release_date/date").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
        coming_soon: d.pointer("/release_date/coming_soon").and_then(Value::as_bool).unwrap_or(false),
        genres: d.get("genres").and_then(Value::as_array).map(|a| a.iter().filter_map(|g| g.get("description")?.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        descriptors: d.pointer("/content_descriptors/ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect()).unwrap_or_default(),
        reviews: d.pointer("/recommendations/total").and_then(Value::as_u64),
        shots: d.get("screenshots").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.get("path_full")?.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        price: d.get("price_overview").and_then(price_from),
        windows: d.pointer("/platforms/windows").and_then(Value::as_bool).unwrap_or(false),
        linux: d.pointer("/platforms/linux").and_then(Value::as_bool).unwrap_or(false),
    }))
}

/// The community tags on a store page (`InitAppTagModal(<appid>, [{"tagid":..,"name":..,"count":..}, ...]`).
/// `None` when the page has no tag block (age gate, error page, a game without tags yet).
pub fn parse_tags(html: &str) -> Option<Vec<Tag>> {
    let at = html.find("InitAppTagModal(")?;
    let rest = &html[at..];
    let open = rest.find('[')?;
    let mut it = serde_json::Deserializer::from_str(&rest[open..]).into_iter::<Value>();
    let arr = it.next()?.ok()?;
    let arr = arr.as_array()?;
    Some(
        arr.iter()
            .filter_map(|t| {
                Some(Tag {
                    id: t.get("tagid").and_then(Value::as_u64).unwrap_or(0) as u32,
                    name: unescape(t.get("name")?.as_str()?),
                    count: t.get("count").and_then(Value::as_u64).unwrap_or(0) as u32,
                })
            })
            .collect(),
    )
}

/// Steam's genres that are also community tags (their ids are what `tagids` carries); `GetItems` has no genre
/// list of its own, so a game's genres are the ones among its tags.
const GENRE_NAMES: [&str; 10] = ["Action", "Adventure", "Casual", "Indie", "Massively Multiplayer", "Racing", "RPG", "Simulation", "Sports", "Strategy"];

/// `IStoreService/GetTagList`: tag id -> English name.
pub fn parse_tag_list(text: &str) -> Result<HashMap<u32, String>> {
    let v: Value = serde_json::from_str(text)?;
    let tags = v.pointer("/response/tags").and_then(Value::as_array).ok_or_else(|| Error::Parse("tag list: no tags".into()))?;
    let out: HashMap<u32, String> = tags.iter().filter_map(|t| Some((u32::try_from(t.get("tagid")?.as_u64()?).ok()?, unescape(t.get("name")?.as_str()?)))).collect();
    if out.is_empty() {
        return Err(Error::Parse("tag list: empty".into()));
    }
    Ok(out)
}

/// What one `GetItems` answer holds.
#[derive(Debug, Default)]
pub struct ItemsAnswer {
    /// Details of the apps Steam knows, in the answer's order.
    pub infos: Vec<AppInfo>,
    /// Community tags of those apps whose every tag id has a name. An app with none at all gets an empty list.
    pub tags: HashMap<u32, Vec<Tag>>,
    /// Apps the answer does not describe (removed, not sold here): ask `appdetails` before giving up on them.
    pub missing: Vec<u32>,
    /// Apps with tag ids that have no name in the tag list: their tags come from the store page.
    pub unnamed: Vec<u32>,
}

fn release_text(v: &Value) -> (Option<String>, bool) {
    let soon = v.pointer("/release/is_coming_soon").and_then(Value::as_bool).unwrap_or(false);
    let Some(at) = v.pointer("/release/steam_release_date").and_then(Value::as_i64).filter(|t| *t > 0) else { return (None, soon) };
    let Some(d) = chrono::DateTime::from_timestamp(at, 0) else { return (None, soon) };
    let text = if soon && v.pointer("/release/coming_soon_display").and_then(Value::as_str) == Some("date_month") { d.format("%b %Y").to_string() } else { d.format("%-d %b, %Y").to_string() };
    (Some(text), soon)
}

fn item_price(v: &Value, currency: &str) -> Option<SteamPrice> {
    let o = v.get("best_purchase_option")?;
    let num = |k: &str| o.get(k).and_then(|x| x.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| x.as_i64()));
    let fin = num("final_price_in_cents")?;
    Some(SteamPrice { currency: currency.to_string(), initial: num("original_price_in_cents").unwrap_or(fin), r#final: fin, discount: o.get("discount_pct").and_then(Value::as_u64).unwrap_or(0) as u32 })
}

/// `IStoreBrowseService/GetItems`: details and community tags of many apps in one answer. `names` maps tag ids to
/// names, `currency` is the price currency of the country asked (when known), `cdn` builds the screenshot URLs.
pub fn parse_items(text: &str, names: &HashMap<u32, String>, currency: Option<&str>, cdn: &str) -> Result<ItemsAnswer> {
    let v: Value = serde_json::from_str(text)?;
    let items = v.pointer("/response/store_items").and_then(Value::as_array).ok_or_else(|| Error::Parse("items: no store_items".into()))?;
    let mut out = ItemsAnswer::default();
    for it in items {
        let Some(appid) = it.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) else { continue };
        if it.get("success").and_then(Value::as_i64) != Some(1) || it.get("name").and_then(Value::as_str).is_none_or(str::is_empty) {
            out.missing.push(appid);
            continue;
        }
        let list = |p: &str| -> Vec<String> { it.pointer(p).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.get("name")?.as_str().map(unescape)).collect()).unwrap_or_default() };
        let mut tags: Vec<Tag> = Vec::new();
        let mut unnamed = false;
        for t in it.get("tags").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
            let Some(id) = t.get("tagid").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) else { continue };
            match names.get(&id) {
                Some(name) => tags.push(Tag { id, name: name.clone(), count: t.get("weight").and_then(Value::as_u64).unwrap_or(0) as u32 }),
                None => unnamed = true,
            }
        }
        tags.sort_by(|a, b| b.count.cmp(&a.count));
        if unnamed {
            out.unnamed.push(appid);
        } else {
            out.tags.insert(appid, tags.clone());
        }
        let genres: Vec<String> = tags.iter().filter(|t| GENRE_NAMES.contains(&t.name.as_str())).map(|t| t.name.clone()).collect();
        let (release, coming_soon) = release_text(it);
        let shots = it
            .pointer("/screenshots/all_ages_screenshots")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .or_else(|| it.pointer("/screenshots/mature_content_screenshots").and_then(Value::as_array))
            .map(|a| a.iter().take(crate::art::MAX_SHOTS).filter_map(|s| Some(format!("{cdn}/store_item_assets/{}", s.get("filename")?.as_str()?.split('?').next()?))).collect())
            .unwrap_or_default();
        out.infos.push(AppInfo {
            appid,
            name: unescape(it.get("name").and_then(Value::as_str).unwrap_or("")),
            kind: match it.get("type").and_then(Value::as_i64).unwrap_or(0) {
                0 => "game",
                1 => "demo",
                4 => "dlc",
                6 => "application",
                _ => "other",
            }
            .to_string(),
            is_free: it.get("is_free").and_then(Value::as_bool).unwrap_or(false),
            short: unescape(it.pointer("/basic_info/short_description").and_then(Value::as_str).unwrap_or("")),
            developers: list("/basic_info/developers"),
            publishers: list("/basic_info/publishers"),
            release,
            coming_soon,
            genres,
            descriptors: it.get("content_descriptorids").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect()).unwrap_or_default(),
            reviews: it.pointer("/reviews/summary_filtered/review_count").and_then(Value::as_u64),
            shots,
            price: currency.and_then(|c| item_price(it, c)),
            windows: it.pointer("/platforms/windows").and_then(Value::as_bool).unwrap_or(false),
            linux: it.pointer("/platforms/steamos_linux").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(out)
}

/// `IWishlistService/GetWishlist`.
pub fn parse_wishlist(text: &str) -> Result<Vec<WishItem>> {
    let v: Value = serde_json::from_str(text)?;
    let items = v.pointer("/response/items");
    match items {
        // A private wishlist answers `{"response":{}}`.
        None => Ok(Vec::new()),
        Some(a) => Ok(a
            .as_array()
            .ok_or_else(|| Error::Parse("wishlist: items is not a list".into()))?
            .iter()
            .filter_map(|i| Some(WishItem { appid: u32::try_from(i.get("appid")?.as_u64()?).ok()?, added: i.get("date_added").and_then(Value::as_i64).unwrap_or(0) }))
            .collect()),
    }
}

/// `ISteamChartsService/GetMostPlayedGames`: (appid, peak players), best first.
pub fn parse_most_played(text: &str) -> Result<Vec<(u32, u64)>> {
    let v: Value = serde_json::from_str(text)?;
    let ranks = v.pointer("/response/ranks").and_then(Value::as_array).ok_or_else(|| Error::Parse("most played: no ranks".into()))?;
    Ok(ranks.iter().filter_map(|r| Some((u32::try_from(r.get("appid")?.as_u64()?).ok()?, r.get("peak_in_game").and_then(Value::as_u64).unwrap_or(0)))).collect())
}

/// How a list is sorted/filtered on the search page.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub term: Option<String>,
    /// `topsellers`, `popularnew`, `popularcomingsoon`, `globaltopsellers`...
    pub filter: Option<&'static str>,
    pub sort_by: Option<&'static str>,
    /// Whole units of the store currency (R100 -> 100).
    pub max_price: Option<u32>,
    pub specials: bool,
    pub tags: Vec<u32>,
    pub count: u32,
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub struct Steam {
    net: Arc<Net>,
}

const PRICE_CHUNK: usize = 40;
/// Apps per `GetItems` request (a 98-app answer with screenshots is about 430 KB).
pub const ITEMS_CHUNK: usize = 50;

impl Steam {
    pub fn new(net: Arc<Net>) -> Steam {
        Steam { net }
    }

    async fn ok(&self, svc: Svc, url: &str, headers: &[(&str, &str)]) -> Result<crate::net::Reply> {
        let r = self.net.get(svc, url, headers).await?;
        if r.status != 200 {
            return Err(Error::Status(r.status));
        }
        Ok(r)
    }

    pub async fn search(&self, q: &SearchQuery, cc: &str) -> Result<Vec<ListItem>> {
        let mut url = format!("{}/search/results/?json=1&cc={}&l=english&category1=998&count={}", self.net.endpoints.steam_store, enc(cc), q.count.clamp(1, 100));
        if let Some(t) = &q.term {
            url.push_str(&format!("&term={}", enc(t)));
        }
        if let Some(f) = q.filter {
            url.push_str(&format!("&filter={f}"));
        }
        if let Some(s) = q.sort_by {
            url.push_str(&format!("&sort_by={s}"));
        }
        if let Some(m) = q.max_price {
            url.push_str(&format!("&maxprice={m}"));
        }
        if q.specials {
            url.push_str("&specials=1");
        }
        if !q.tags.is_empty() {
            url.push_str(&format!("&tags={}", q.tags.iter().map(u32::to_string).collect::<Vec<_>>().join(",")));
        }
        parse_search(&self.ok(Svc::Steam, &url, &[]).await?.text())
    }

    pub async fn featured(&self, cc: &str) -> Result<Featured> {
        let url = format!("{}/api/featuredcategories?cc={}&l=english", self.net.endpoints.steam_store, enc(cc));
        parse_featured(&self.ok(Svc::Steam, &url, &[]).await?.text())
    }

    /// Prices for many apps; asks in chunks of 40 ids.
    pub async fn prices(&self, ids: &[u32], cc: &str) -> Result<HashMap<u32, PriceLookup>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(PRICE_CHUNK) {
            let list = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let url = format!("{}/api/appdetails?appids={list}&filters=price_overview&cc={}", self.net.endpoints.steam_store, enc(cc));
            out.extend(parse_prices(&self.ok(Svc::Steam, &url, &[]).await?.text())?);
        }
        Ok(out)
    }

    /// Full details of one app. `Ok(None)`: Steam has nothing for it.
    pub async fn details(&self, appid: u32, cc: &str) -> Result<Option<AppInfo>> {
        let url = format!("{}/api/appdetails?appids={appid}&cc={}&l=english", self.net.endpoints.steam_store, enc(cc));
        parse_app_details(&self.ok(Svc::Steam, &url, &[]).await?.text(), appid)
    }

    /// The tag id -> name table (one request; the answer changes rarely).
    pub async fn tag_names(&self) -> Result<HashMap<u32, String>> {
        let url = format!("{}/IStoreService/GetTagList/v1/?language=english", self.net.endpoints.steam_api);
        parse_tag_list(&self.ok(Svc::Steam, &url, &[]).await?.text())
    }

    /// Details and community tags of many apps in one request (`GetItems`, keyless). `currency` is what prices
    /// in `cc` are in, when known. Asks in chunks of [`ITEMS_CHUNK`].
    pub async fn items(&self, ids: &[u32], cc: &str, names: &HashMap<u32, String>, currency: Option<&str>) -> Result<ItemsAnswer> {
        let mut out = ItemsAnswer::default();
        for chunk in ids.chunks(ITEMS_CHUNK) {
            let input = serde_json::json!({
                "ids": chunk.iter().map(|id| serde_json::json!({ "appid": id })).collect::<Vec<_>>(),
                "context": { "language": "english", "country_code": cc, "steam_realm": 1 },
                "data_request": { "include_basic_info": true, "include_tag_count": 30, "include_reviews": true, "include_release": true, "include_platforms": true, "include_screenshots": true },
            });
            let url = format!("{}/IStoreBrowseService/GetItems/v1/?input_json={}", self.net.endpoints.steam_api, enc(&input.to_string()));
            let a = parse_items(&self.ok(Svc::Steam, &url, &[]).await?.text(), names, currency, &self.net.endpoints.steam_cdn)?;
            out.infos.extend(a.infos);
            out.tags.extend(a.tags);
            out.missing.extend(a.missing);
            out.unnamed.extend(a.unnamed);
        }
        Ok(out)
    }

    /// Community tags from the store page (age-gate cookies set, so mature games answer too).
    pub async fn tags(&self, appid: u32) -> Result<Option<Vec<Tag>>> {
        let url = format!("{}/app/{appid}/?l=english", self.net.endpoints.steam_store);
        let r = self.ok(Svc::Steam, &url, &[("Cookie", AGE_GATE_COOKIE), ("Accept-Language", "en")]).await?;
        self.net.note_tag_page();
        Ok(parse_tags(&r.text()))
    }

    pub async fn wishlist(&self, steamid: u64) -> Result<Vec<WishItem>> {
        let url = format!("{}/IWishlistService/GetWishlist/v1/?steamid={steamid}", self.net.endpoints.steam_api);
        parse_wishlist(&self.ok(Svc::Steam, &url, &[]).await?.text())
    }

    pub async fn most_played(&self) -> Result<Vec<(u32, u64)>> {
        let url = format!("{}/ISteamChartsService/GetMostPlayedGames/v1/", self.net.endpoints.steam_api);
        parse_most_played(&self.ok(Svc::Steam, &url, &[]).await?.text())
    }
}

/// Where Steam's art lives. Deterministic per app id, so no request is needed to know the URLs.
pub fn art_url(cdn: &str, appid: u32, file: &str) -> String {
    format!("{cdn}/store_item_assets/steam/apps/{appid}/{file}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn search_gives_apps_in_order_and_skips_packages() {
        let items = parse_search(&fx("search_under.json")).unwrap();
        assert_eq!(items[0].appid, 4954910);
        assert_eq!(items[1].appid, 990080);
        assert!(items.len() >= 6);
        assert!(items.iter().all(|i| i.price.is_none() && !i.name.is_empty()));
        // the fixture has a `subs` (package) entry: it must not become an app
        assert!(!items.iter().any(|i| i.appid == 320795));
        let top = parse_search(&fx("search_topsellers.json")).unwrap();
        assert_eq!(top[1].name, "Dota 2");
        assert_eq!(top[1].appid, 570);
    }

    #[test]
    fn featured_has_prices_and_discounts() {
        let f = parse_featured(&fx("featured.json")).unwrap();
        assert!(!f.specials.is_empty());
        let s = &f.specials[0];
        let p = s.price.as_ref().unwrap();
        assert!(p.discount > 0 && p.initial > p.r#final && p.currency == "ZAR");
        assert!(s.discount_expires.is_some());
        assert!(!f.top_sellers.is_empty() && !f.new_releases.is_empty());
        assert!(parse_featured("{\"a\":1}").is_err());
    }

    #[test]
    fn bulk_prices_tell_free_from_priced() {
        let p = parse_prices(&fx("prices_multi.json")).unwrap();
        assert_eq!(p[&730], PriceLookup::NoPrice, "free games answer with an empty list");
        match &p[&990080] {
            PriceLookup::Priced(x) => assert_eq!((x.initial, x.r#final, x.discount), (99900, 9990, 90)),
            other => panic!("{other:?}"),
        }
        // a failed id is left out rather than read as "free"
        assert!(parse_prices(&fx("appdetails_failed.json")).unwrap().is_empty());
    }

    #[test]
    fn details_read_the_fields_the_store_shows() {
        let a = parse_app_details(&fx("appdetails_1621690.json"), 1621690).unwrap().unwrap();
        assert_eq!(a.name, "Core Keeper");
        assert!(a.is_game() && !a.is_free && !a.coming_soon);
        assert_eq!(a.developers, ["Pugstorm"]);
        assert_eq!(a.publishers, ["Fireshine Games", "bilibili"]);
        assert_eq!(a.genres[0], "Action");
        assert_eq!(a.release.as_deref(), Some("27 Aug, 2024"));
        assert_eq!(a.reviews, Some(50830));
        assert_eq!(a.price.as_ref().unwrap().discount, 40);
        assert!(a.shots.len() == 4 && a.shots[0].contains("1920x1080"));
        assert!(a.descriptors.is_empty() && a.linux);
        let c = parse_app_details(&fx("appdetails_1091500.json"), 1091500).unwrap().unwrap();
        assert_eq!(c.descriptors, [1, 2, 5]);
        assert!(parse_app_details(&fx("appdetails_failed.json"), 999999999).unwrap().is_none());
    }

    #[test]
    fn tags_come_from_the_page_and_age_gate_gives_none() {
        let t = parse_tags(&fx("page_1621690.html")).unwrap();
        assert_eq!(t[0], Tag { id: 1695, name: "Open World".into(), count: 1363 });
        assert!(t.iter().any(|x| x.name == "Survival"));
        let m = parse_tags(&fx("page_4771580.html")).unwrap();
        assert_eq!(m[0].name, "Sexual Content");
        assert!(m.iter().any(|x| x.name == "Hentai"));
        assert_eq!(parse_tags(&fx("page_agegate.html")), None);
        assert_eq!(parse_tags("InitAppTagModal( 1, [], [] );"), Some(vec![]));
    }

    #[test]
    fn bulk_items_give_details_and_named_tags() {
        let names = parse_tag_list(&fx("tag_list.json")).unwrap();
        assert_eq!(names[&1695], "Open World");
        assert!(parse_tag_list("{\"response\":{}}").is_err());
        let a = parse_items(&fx("items_multi.json"), &names, Some("ZAR"), "https://cdn.test").unwrap();
        assert_eq!(a.missing, [999999999], "an id Steam does not describe is handed back");
        assert!(a.unnamed.is_empty() && a.infos.len() == 5);
        let ck = a.infos.iter().find(|i| i.appid == 1621690).unwrap();
        assert_eq!(ck.name, "Core Keeper");
        assert!(ck.is_game() && !ck.is_free && !ck.coming_soon && ck.windows && ck.linux);
        assert_eq!(ck.developers, ["Pugstorm"]);
        assert_eq!(ck.publishers, ["Fireshine Games", "bilibili"]);
        assert_eq!(ck.release.as_deref(), Some("27 Aug, 2024"));
        assert_eq!(ck.reviews, Some(50684));
        assert_eq!(ck.genres, ["Action", "Adventure", "Indie"], "the Steam genres among its tags, most voted first");
        let p = ck.price.as_ref().unwrap();
        assert_eq!((p.currency.as_str(), p.initial, p.r#final, p.discount), ("ZAR", 18500, 11100, 40));
        assert!(ck.shots.len() == 3 && ck.shots[0].starts_with("https://cdn.test/store_item_assets/steam/apps/1621690/") && !ck.shots[0].contains('?'));
        assert!(ck.descriptors.is_empty());
        let t = &a.tags[&1621690];
        assert_eq!(t[0], Tag { id: 1695, name: "Open World".into(), count: 587 });
        assert!(t.windows(2).all(|w| w[0].count >= w[1].count));
        // content descriptors and the Linux flag (Cyberpunk: no SteamOS build)
        let cp = a.infos.iter().find(|i| i.appid == 1091500).unwrap();
        assert_eq!(cp.descriptors, [1, 2, 5]);
        assert!(cp.windows && !cp.linux);
        // a game with no descriptors but adult tags is still judged by its tags
        let maids = a.infos.iter().find(|i| i.appid == 4771580).unwrap();
        assert!(maids.descriptors.is_empty() && crate::filters::adult_by_tags(&a.tags[&4771580]));
        // without a known currency there is no price (the bulk price lookup has it)
        assert!(parse_items(&fx("items_multi.json"), &names, None, "").unwrap().infos.iter().all(|i| i.price.is_none()));
    }

    #[test]
    fn bulk_items_handle_unreleased_free_dlc_and_unnamed_tags() {
        let json = r#"{"response":{"store_items":[
          {"id":10,"success":1,"name":"Soon","type":0,"tags":[{"tagid":19,"weight":5},{"tagid":777,"weight":4}],"release":{"steam_release_date":1893456000,"is_coming_soon":true,"coming_soon_display":"date_month"}},
          {"id":11,"success":1,"name":"Freebie","type":0,"is_free":true,"tags":[],"release":{"steam_release_date":1500000000}},
          {"id":12,"success":1,"name":"Add-on","type":4},
          {"id":13,"success":15,"visible":false}]}}"#;
        let names: HashMap<u32, String> = HashMap::from([(19, "Action".to_string())]);
        let a = parse_items(json, &names, Some("USD"), "").unwrap();
        assert_eq!(a.unnamed, [10], "a tag id without a name sends the app to the store page for its tags");
        assert!(!a.tags.contains_key(&10) && a.tags[&11].is_empty() && a.tags[&12].is_empty());
        let soon = &a.infos[0];
        assert!(soon.coming_soon && soon.release.as_deref() == Some("Jan 2030"));
        assert_eq!(soon.genres, ["Action"]);
        assert!(a.infos[1].is_free && a.infos[1].price.is_none() && !a.infos[1].coming_soon);
        assert_eq!(a.infos[2].kind, "dlc");
        assert_eq!(a.missing, [13]);
    }

    #[test]
    fn wishlist_and_charts() {
        let w = parse_wishlist(&fx("wishlist.json")).unwrap();
        assert_eq!(w[0], WishItem { appid: 221100, added: 1689278026 });
        assert!(parse_wishlist("{\"response\":{}}").unwrap().is_empty(), "a private wishlist is empty, not an error");
        let m = parse_most_played(&fx("mostplayed.json")).unwrap();
        assert_eq!(m[0].0, 730);
        assert_eq!(steam_id64(12345678), 76561197972611406);
    }

    #[test]
    fn query_encoding_is_safe() {
        assert_eq!(enc("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }
}
