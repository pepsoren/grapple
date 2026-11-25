//! A fake Steam/Epic/FX/ITAD on a local port, built from a small controllable world. The answers have the
//! shapes recorded in `tests/fixtures` (the parser tests read the real recordings).

#![allow(dead_code)]

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use grapple_store::net::{Endpoints, LimitConfig};
use grapple_store::secrets::MemoryKeys;
use grapple_store::epic_account::{EpicSession, EpicSessionProvider};
use grapple_store::secrets::ApiKey;
use grapple_store::{Friend, Library, OwnedGame, StoreConfig, StoreEvent, StoreService};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct G {
    pub id: u32,
    pub name: String,
    pub genres: Vec<&'static str>,
    pub desc: Vec<u32>,
    pub tags: Vec<(&'static str, u32)>,
    /// ZAR cents
    pub initial: i64,
    pub fin: i64,
    pub free: bool,
    pub soon: bool,
    pub kind: &'static str,
    pub cover: bool,
}

pub fn game(id: u32, name: &str) -> G {
    G { id, name: name.into(), genres: vec!["Action"], desc: vec![], tags: vec![("Action", 100)], initial: 10000, fin: 10000, free: false, soon: false, kind: "game", cover: true }
}

impl G {
    pub fn genres(mut self, g: &[&'static str]) -> G {
        self.genres = g.to_vec();
        self
    }
    pub fn desc(mut self, d: &[u32]) -> G {
        self.desc = d.to_vec();
        self
    }
    pub fn tags(mut self, t: &[(&'static str, u32)]) -> G {
        self.tags = t.to_vec();
        self
    }
    pub fn price(mut self, initial: i64, fin: i64) -> G {
        self.initial = initial;
        self.fin = fin;
        self
    }
    pub fn soon(mut self) -> G {
        self.soon = true;
        self
    }
    pub fn kind(mut self, k: &'static str) -> G {
        self.kind = k;
        self
    }
    pub fn no_cover(mut self) -> G {
        self.cover = false;
        self
    }
    fn discount(&self) -> i64 {
        if self.initial > 0 { 100 - 100 * self.fin / self.initial } else { 0 }
    }
}

#[derive(Default)]
pub struct World {
    pub games: BTreeMap<u32, G>,
    pub top: Vec<u32>,
    pub new: Vec<u32>,
    pub under: Vec<u32>,
    pub wishlist: Vec<u32>,
}

pub struct Srv {
    pub world: Mutex<World>,
    pub counts: Mutex<HashMap<String, u64>>,
    /// answer this many Steam requests with 429
    pub limit_next: AtomicU32,
    pub itad_log: Mutex<Vec<(String, bool, String)>>,
    pub itad_epic_price: Mutex<Option<(i64, i64, u32)>>,
    pub tag_requests_without_cookie: AtomicU64,
    /// answer `GetItems` with 404 (an old Steam without the bulk service)
    pub no_items: std::sync::atomic::AtomicBool,
    /// a tag name the tag list leaves out (its ids stay unnamed in `GetItems` answers)
    pub unlisted_tag: Mutex<Option<&'static str>>,
    pub fx_zar: Mutex<f64>,
    pub epic: Mutex<EpicWorld>,
}

/// An offer in the fake Epic catalog. Amounts are USD cents.
#[derive(Clone)]
pub struct EOffer {
    pub ns: String,
    pub id: String,
    pub title: String,
    pub offer_type: &'static str,
    pub makers: Vec<&'static str>,
    pub original: i64,
    pub now: i64,
}

pub fn eoffer(ns: &str, id: &str, title: &str, makers: &[&'static str], original: i64, now: i64) -> EOffer {
    EOffer { ns: ns.into(), id: id.into(), title: title.into(), offer_type: "BASE_GAME", makers: makers.to_vec(), original, now }
}

pub struct EpicWorld {
    pub offers: Vec<EOffer>,
    /// the only bearer token Epic accepts
    pub valid_token: String,
    pub price_engine_ok: bool,
    /// what the services saw: (route, token, detail)
    pub log: Vec<(String, String, String)>,
    pub billing_country: String,
}

impl Default for EpicWorld {
    fn default() -> Self {
        EpicWorld { offers: vec![], valid_token: "good".into(), price_engine_ok: true, log: vec![], billing_country: "ZA".into() }
    }
}

impl Srv {
    pub fn count(&self, name: &str) -> u64 {
        *self.counts.lock().unwrap().get(name).unwrap_or(&0)
    }
    pub fn steam_total(&self) -> u64 {
        ["search", "featured", "appdetails", "prices", "tags", "wishlist", "mostplayed", "items", "taglist"].iter().map(|n| self.count(n)).sum()
    }
    fn hit(&self, name: &str) {
        *self.counts.lock().unwrap().entry(name.into()).or_insert(0) += 1;
    }
    fn limited(&self) -> bool {
        self.limit_next.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
    }
}

type S = State<Arc<Srv>>;
type Q = Query<HashMap<String, String>>;

fn cc_currency(q: &HashMap<String, String>) -> (&'static str, i64) {
    match q.get("cc").map(String::as_str) {
        Some("ZA") | None => ("ZAR", 1),
        _ => ("USD", 16), // dollars: the world's rand divided by 16
    }
}

fn price_json(g: &G, q: &HashMap<String, String>) -> Value {
    let (cur, div) = cc_currency(q);
    let (i, f) = (g.initial / div, g.fin / div);
    json!({"currency": cur, "initial": i, "final": f, "discount_percent": g.discount(),
           "initial_formatted": "", "final_formatted": format!("{cur} {:.2}", f as f64 / 100.0)})
}

fn too_many() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")], "slow down").into_response()
}

fn search_item(g: &G) -> Value {
    json!({"name": g.name, "logo": format!("https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{}/abcdef/capsule_sm_120.jpg?t=1", g.id)})
}

async fn search(State(s): S, Query(q): Q) -> Response {
    s.hit("search");
    if s.limited() {
        return too_many();
    }
    let w = s.world.lock().unwrap();
    let ids: Vec<u32> = if let Some(t) = q.get("term") {
        let t = t.to_lowercase();
        w.games.values().filter(|g| g.name.to_lowercase().contains(&t) || g.genres.iter().any(|x| x.to_lowercase() == t)).map(|g| g.id).collect()
    } else if let Some(tag) = q.get("tags") {
        let want: u32 = tag.split(',').next().unwrap().parse().unwrap_or(0);
        w.games.values().filter(|g| g.tags.iter().any(|(n, _)| tag_id(n) == want)).map(|g| g.id).collect()
    } else if q.contains_key("maxprice") {
        let max: i64 = q["maxprice"].parse().unwrap();
        w.under.iter().copied().filter(|id| w.games[id].fin <= max * 100).collect()
    } else if q.get("sort_by").map(String::as_str) == Some("Released_DESC") {
        w.new.clone()
    } else {
        w.top.clone()
    };
    let items: Vec<Value> = ids.iter().filter_map(|i| w.games.get(i)).map(search_item).collect();
    axum::Json(json!({"desc": "", "items": items})).into_response()
}

pub fn tag_id(name: &str) -> u32 {
    match name {
        "Survival" => 1662,
        "Co-op" => 1685,
        "Action" => 19,
        "Puzzle" => 1664,
        "Sexual Content" => 12095,
        "Hentai" => 9130,
        "Crafting" => 1702,
        "Open World" => 1695,
        "Shooter" => 1774,
        "RPG" => 122,
        // Any other name gets a stable id of its own (the tag list below names them all).
        other => 100_000 + other.bytes().map(u32::from).fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b)) % 800_000,
    }
}

async fn tag_list(State(s): S) -> Response {
    s.hit("taglist");
    if s.limited() {
        return too_many();
    }
    let w = s.world.lock().unwrap();
    let mut names: Vec<&str> = w.games.values().flat_map(|g| g.tags.iter().map(|(n, _)| *n).chain(g.genres.iter().copied())).collect();
    names.sort();
    names.dedup();
    let unlisted = *s.unlisted_tag.lock().unwrap();
    names.retain(|n| Some(*n) != unlisted);
    let tags: Vec<Value> = names.iter().map(|n| json!({"tagid": tag_id(n), "name": n})).collect();
    axum::Json(json!({"response": {"version_hash": "1", "tags": tags}})).into_response()
}

/// `IStoreBrowseService/GetItems/v1`: the bulk details (and community tags) the store asks for.
async fn get_items(State(s): S, Query(q): Q) -> Response {
    if s.no_items.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    s.hit("items");
    if s.limited() {
        return too_many();
    }
    let input: Value = serde_json::from_str(q.get("input_json").map(String::as_str).unwrap_or("{}")).unwrap_or_default();
    let cc = input.pointer("/context/country_code").and_then(Value::as_str).unwrap_or("ZA").to_string();
    let mut qq = HashMap::new();
    qq.insert("cc".to_string(), cc);
    let w = s.world.lock().unwrap();
    let items: Vec<Value> = input["ids"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i["appid"].as_u64())
        .map(|id| {
            let Some(g) = w.games.get(&(id as u32)) else { return json!({"item_type": 0, "id": id, "success": 15, "visible": false, "name": "", "appid": id}) };
            let (cur, div) = cc_currency(&qq);
            let _ = cur;
            let mut it = json!({
                "item_type": 0, "id": g.id, "success": 1, "visible": true, "name": g.name, "appid": g.id,
                "type": match g.kind { "game" => 0, "demo" => 1, "dlc" => 4, _ => 6 },
                "is_free": g.free,
                // Steam's genres are among a game's tags too (that is where the store reads them from).
                "tags": g.tags.iter().copied().chain(g.genres.iter().filter(|n| !g.tags.iter().any(|(t, _)| t == *n)).map(|n| (*n, 50))).map(|(n, c)| json!({"tagid": tag_id(n), "weight": c})).collect::<Vec<_>>(),
                "reviews": {"summary_filtered": {"review_count": 5000, "percent_positive": 90}},
                "basic_info": {"short_description": format!("About {}.", g.name), "developers": [{"name": "Dev"}], "publishers": [{"name": "Pub"}]},
                "release": {"steam_release_date": 1_735_689_600, "is_coming_soon": g.soon},
                "platforms": {"windows": true, "steamos_linux": true},
                "screenshots": {"all_ages_screenshots": [{"filename": format!("steam/apps/{}/ss_one.1920x1080.jpg?t=1", g.id), "ordinal": 1}]},
            });
            if !g.desc.is_empty() {
                it["content_descriptorids"] = json!(g.desc);
            }
            if !g.free && !g.soon {
                it["best_purchase_option"] = json!({"final_price_in_cents": (g.fin / div).to_string(), "original_price_in_cents": (g.initial / div).to_string(), "discount_pct": g.discount()});
            }
            it
        })
        .collect();
    axum::Json(json!({"response": {"store_items": items}})).into_response()
}

fn featured_item(g: &G) -> Value {
    json!({"id": g.id, "type": 0, "name": g.name, "discounted": g.discount() > 0, "discount_percent": g.discount(),
           "original_price": g.initial, "final_price": g.fin, "currency": "ZAR", "discount_expiration": 1_900_000_000})
}

async fn featured(State(s): S) -> Response {
    s.hit("featured");
    if s.limited() {
        return too_many();
    }
    let w = s.world.lock().unwrap();
    let specials: Vec<Value> = w.games.values().filter(|g| g.discount() > 0).map(featured_item).collect();
    let tops: Vec<Value> = w.top.iter().take(6).filter_map(|i| w.games.get(i)).map(featured_item).collect();
    let news: Vec<Value> = w.new.iter().take(6).filter_map(|i| w.games.get(i)).map(featured_item).collect();
    axum::Json(json!({
        "specials": {"id": "cat_specials", "name": "Specials", "items": specials},
        "top_sellers": {"id": "cat_topsellers", "name": "Top Sellers", "items": tops},
        "new_releases": {"id": "cat_newreleases", "name": "New", "items": news},
        "coming_soon": {"id": "cat_comingsoon", "name": "Soon", "items": []},
        "status": 1
    }))
    .into_response()
}

async fn appdetails(State(s): S, Query(q): Q, headers: HeaderMap) -> Response {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("127.0.0.1").to_string();
    if s.limited() {
        s.hit("limited");
        return too_many();
    }
    let ids: Vec<u32> = q.get("appids").map(|a| a.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let w = s.world.lock().unwrap();
    if q.get("filters").map(String::as_str) == Some("price_overview") {
        s.hit("prices");
        let mut m = serde_json::Map::new();
        for id in ids {
            match w.games.get(&id) {
                Some(g) if g.free || g.soon => {
                    m.insert(id.to_string(), json!({"success": true, "data": []}));
                }
                Some(g) => {
                    m.insert(id.to_string(), json!({"success": true, "data": {"price_overview": price_json(g, &q)}}));
                }
                None => {
                    m.insert(id.to_string(), json!({"success": false}));
                }
            }
        }
        return axum::Json(Value::Object(m)).into_response();
    }
    s.hit("appdetails");
    let id = ids[0];
    let Some(g) = w.games.get(&id) else { return axum::Json(json!({ id.to_string(): {"success": false} })).into_response() };
    let mut data = json!({
        "type": g.kind, "name": g.name, "steam_appid": g.id, "is_free": g.free, "short_description": format!("About {}.", g.name),
        "developers": ["Dev"], "publishers": ["Pub"], "genres": g.genres.iter().enumerate().map(|(i, n)| json!({"id": i.to_string(), "description": n})).collect::<Vec<_>>(),
        "release_date": {"coming_soon": g.soon, "date": "1 Jan, 2025"},
        "content_descriptors": {"ids": g.desc, "notes": null},
        "recommendations": {"total": 5000},
        "screenshots": [{"id": 0, "path_full": format!("http://{host}/cdn/store_item_assets/steam/apps/{}/ss_one.1920x1080.jpg", g.id)}],
        "platforms": {"windows": true, "mac": false, "linux": true},
    });
    if !g.free && !g.soon {
        data["price_overview"] = price_json(g, &q);
    }
    axum::Json(json!({ id.to_string(): {"success": true, "data": data} })).into_response()
}

async fn app_page(State(s): S, Path(id): Path<u32>, headers: HeaderMap) -> Response {
    s.hit("tags");
    s.hit(&format!("tagpage:{id}"));
    if s.limited() {
        return too_many();
    }
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !cookie.contains("birthtime=") || !cookie.contains("mature_content=1") {
        s.tag_requests_without_cookie.fetch_add(1, Ordering::SeqCst);
        return "<html><body>Please enter your birth date</body></html>".into_response();
    }
    let w = s.world.lock().unwrap();
    let Some(g) = w.games.get(&id) else { return StatusCode::NOT_FOUND.into_response() };
    let tags: Vec<Value> = g.tags.iter().map(|(n, c)| json!({"tagid": tag_id(n), "name": n, "count": c, "browseable": true})).collect();
    format!("<html><script>\nInitAppTagModal( {id},\n\t\t\t\t{},\n\t\t\t\t[], \"x\" );\n</script></html>", Value::Array(tags)).into_response()
}

async fn wishlist(State(s): S, Query(q): Q) -> Response {
    s.hit("wishlist");
    if s.limited() {
        return too_many();
    }
    assert!(q.get("steamid").is_some_and(|v| v.len() == 17));
    let w = s.world.lock().unwrap();
    let items: Vec<Value> = w.wishlist.iter().enumerate().map(|(i, id)| json!({"appid": id, "priority": 0, "date_added": 1_700_000_000 + i as i64})).collect();
    axum::Json(json!({"response": {"items": items}})).into_response()
}

async fn mostplayed(State(s): S) -> Response {
    s.hit("mostplayed");
    axum::Json(json!({"response": {"ranks": [{"rank": 1, "appid": 730, "peak_in_game": 1000}]}})).into_response()
}

fn tiny_jpeg() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
    v.extend_from_slice(&[0u8; 40]);
    v
}

fn tiny_png() -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&[0u8; 40]);
    v
}

async fn cdn(State(s): S, Path((id, file)): Path<(u32, String)>) -> Response {
    s.hit(&format!("cdn:{file}"));
    s.hit("cdn");
    let w = s.world.lock().unwrap();
    let Some(g) = w.games.get(&id) else { return StatusCode::NOT_FOUND.into_response() };
    match file.as_str() {
        "library_600x900.jpg" if !g.cover => StatusCode::NOT_FOUND.into_response(),
        "logo.png" => ([(header::CONTENT_TYPE, "image/png")], tiny_png()).into_response(),
        "library_600x900.jpg" | "header.jpg" | "library_hero.jpg" | "ss_one.1920x1080.jpg" => ([(header::CONTENT_TYPE, "image/jpeg")], tiny_jpeg()).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn epic_free(State(s): S) -> Response {
    s.hit("epic");
    let body = std::fs::read_to_string(format!("{}/tests/fixtures/epic_free.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn epic_img(State(s): S) -> Response {
    s.hit("epicimg");
    ([(header::CONTENT_TYPE, "image/jpeg")], tiny_jpeg()).into_response()
}

async fn fx(State(s): S) -> Response {
    s.hit("fx");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/tests/fixtures/fx.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    v["rates"]["ZAR"] = json!(*s.fx_zar.lock().unwrap());
    axum::Json(v).into_response()
}

async fn itad_lookup(State(s): S, headers: HeaderMap, axum::Json(body): axum::Json<Vec<String>>) -> Response {
    s.hit("itad");
    s.itad_log.lock().unwrap().push(("lookup".into(), headers.contains_key("itad-api-key"), String::new()));
    let m: serde_json::Map<String, Value> = body.iter().map(|k| (k.clone(), json!(format!("gid-{}", k.trim_start_matches("app/"))))).collect();
    axum::Json(Value::Object(m)).into_response()
}

async fn itad_prices(State(s): S, headers: HeaderMap, Query(q): Q, axum::Json(body): axum::Json<Vec<String>>) -> Response {
    s.hit("itad");
    s.itad_log.lock().unwrap().push(("prices".into(), headers.contains_key("itad-api-key"), format!("{q:?}")));
    let epic = *s.itad_epic_price.lock().unwrap();
    let cur = if q.get("country").map(String::as_str) == Some("ZA") { "ZAR" } else { "USD" };
    let out: Vec<Value> = body
        .iter()
        .map(|gid| {
            let deals = match epic {
                Some((orig, price, cut)) => vec![json!({"shop": {"id": 16, "name": "Epic Game Store"},
                    "price": {"amount": price as f64 / 100.0, "amountInt": price, "currency": cur},
                    "regular": {"amount": orig as f64 / 100.0, "amountInt": orig, "currency": cur},
                    "cut": cut, "url": format!("https://itad.example/link/{gid}")})],
                None => vec![],
            };
            json!({"id": gid, "historyLow": {"all": null, "y1": null, "m3": null}, "deals": deals})
        })
        .collect();
    axum::Json(Value::Array(out)).into_response()
}

pub struct Harness {
    pub srv: Arc<Srv>,
    pub base: String,
    pub dir: tempfile::TempDir,
    pub lib: Arc<TestLib>,
    pub events: Arc<Mutex<Vec<StoreEvent>>>,
    pub store: StoreService,
    pub keys: Arc<MemoryKeys>,
}

pub struct TestLib {
    pub owned: Mutex<Vec<OwnedGame>>,
    pub friends: Mutex<Vec<Friend>>,
    pub steam_id: Mutex<Option<u64>>,
    pub region: Mutex<Option<(String, &'static str)>>,
}

impl Library for TestLib {
    fn owned(&self) -> Vec<OwnedGame> {
        self.owned.lock().unwrap().clone()
    }
    fn friends(&self) -> Vec<Friend> {
        self.friends.lock().unwrap().clone()
    }
    fn steam_id64(&self) -> Option<u64> {
        *self.steam_id.lock().unwrap()
    }
    fn steam_region(&self) -> Option<(String, &'static str)> {
        self.region.lock().unwrap().clone()
    }
}

pub fn owned(id: &str, name: &str, minutes: u64) -> OwnedGame {
    OwnedGame { id: id.into(), name: name.into(), playtime_min: minutes, last_played: Some(grapple_now() - 86_400), added_at: None, icon: Some(format!("grapple://art/{id}/icon")), epic_namespace: None }
}

pub fn grapple_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub fn fast_limits() -> (LimitConfig, LimitConfig, LimitConfig) {
    let l = LimitConfig { max: 10_000, window: Duration::from_secs(1), concurrency: 3, min_gap: Duration::ZERO, backoff_base: Duration::from_millis(200), backoff_max: Duration::from_millis(400) };
    (l, l, l)
}

pub async fn harness(world: World) -> Harness {
    harness_with(world, fast_limits()).await
}

pub async fn harness_with(world: World, limits: (LimitConfig, LimitConfig, LimitConfig)) -> Harness {
    let srv = Arc::new(Srv {
        world: Mutex::new(world),
        counts: Mutex::new(HashMap::new()),
        limit_next: AtomicU32::new(0),
        itad_log: Mutex::new(vec![]),
        itad_epic_price: Mutex::new(None),
        tag_requests_without_cookie: AtomicU64::new(0),
        no_items: std::sync::atomic::AtomicBool::new(false),
        unlisted_tag: Mutex::new(None),
        fx_zar: Mutex::new(16.0),
        epic: Mutex::new(EpicWorld::default()),
    });
    let app = Router::new()
        .route("/steam-store/search/results/", get(search))
        .route("/steam-store/api/featuredcategories", get(featured))
        .route("/steam-store/api/appdetails", get(appdetails))
        .route("/steam-store/app/{id}/", get(app_page))
        .route("/steam-api/IStoreBrowseService/GetItems/v1/", get(get_items))
        .route("/steam-api/IStoreService/GetTagList/v1/", get(tag_list))
        .route("/steam-api/IWishlistService/GetWishlist/v1/", get(wishlist))
        .route("/steam-api/ISteamChartsService/GetMostPlayedGames/v1/", get(mostplayed))
        .route("/cdn/store_item_assets/steam/apps/{id}/{file}", get(cdn))
        .route("/cdn/epic/{name}", get(epic_img))
        .route("/epic/freeGamesPromotions", get(epic_free))
        .route("/fx", get(fx))
        .route("/epic-gql/graphql", post(epic_gql))
        .route("/epic-price/priceengine/api/shared/offers/price", post(epic_price))
        .route("/epic-catalog/catalog/api/shared/bulk/offers", get(epic_catalog))
        .route("/epic-launcher/launcher/api/public/payment/accounts/{acct}/billingaccounts/default", get(epic_billing))
        .route("/itad/lookup/id/shop/61/v1", post(itad_lookup))
        .route("/itad/games/prices/v3", post(itad_prices))
        .with_state(srv.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let lib = Arc::new(TestLib { owned: Mutex::new(vec![]), friends: Mutex::new(vec![]), steam_id: Mutex::new(Some(76561197972611406)), region: Mutex::new(Some(("ZA".into(), "steam-config"))) });
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev2 = events.clone();
    let keys = Arc::new(MemoryKeys::new());
    let cfg = StoreConfig {
        data_dir: dir.path().join("data"),
        art_dir: dir.path().join("art"),
        endpoints: Endpoints::local(&base),
        steam_limit: limits.0,
        light_limit: limits.1,
        cdn_limit: limits.2,
        windows_urls: false,
    };
    let store = StoreService::new(cfg, lib.clone(), keys.clone(), Arc::new(move |e| ev2.lock().unwrap().push(e)), tokio::runtime::Handle::current()).unwrap();
    Harness { srv, base, dir, lib, events, store, keys }
}

impl Harness {
    pub fn sales(&self) -> Vec<grapple_store::SaleAlert> {
        self.events.lock().unwrap().iter().filter_map(|e| if let StoreEvent::Sale(s) = e { Some(s.clone()) } else { None }).collect()
    }
    pub fn changed_count(&self) -> usize {
        self.events.lock().unwrap().iter().filter(|e| matches!(e, StoreEvent::Changed(_))).count()
    }
}

/// A world of `n` plain games plus the interesting ones from `extra`.
pub fn world_with(extra: Vec<G>) -> World {
    let mut w = World::default();
    for g in extra {
        w.games.insert(g.id, g);
    }
    w
}

// ----------------------------------------------------------------------------------------------- fake Epic

fn bearer(h: &HeaderMap) -> String {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").to_string()
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, axum::Json(json!({"errorCode": "errors.com.epicgames.common.authentication.token_verification_failed"}))).into_response()
}

fn epic_el(o: &EOffer, cc: &str) -> Value {
    let mut attrs: Vec<Value> = vec![];
    for (i, m) in o.makers.iter().enumerate().skip(1) {
        attrs.push(json!({"key": if i == 1 { "developerName" } else { "publisherName" }, "value": m}));
    }
    json!({"title": o.title, "id": o.id, "namespace": o.ns, "offerType": o.offer_type, "isCodeRedemptionOnly": false,
           "seller": {"id": "s", "name": o.makers.first().copied().unwrap_or("")}, "customAttributes": attrs,
           "keyImages": [], "offerMappings": [{"pageSlug": format!("slug-{}", o.id), "pageType": "productHome"}],
           "price": {"totalPrice": {"discountPrice": o.now, "originalPrice": o.original, "currencyCode": "USD", "currencyInfo": {"decimals": 2}}, "country": cc},
           "promotions": null})
}

async fn epic_gql(State(s): S, headers: HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    s.hit("gql");
    let mut e = s.epic.lock().unwrap();
    let kw = body.pointer("/variables/keywords").and_then(Value::as_str).unwrap_or("").to_string();
    let cc = body.pointer("/variables/country").and_then(Value::as_str).unwrap_or("").to_string();
    e.log.push(("gql".into(), bearer(&headers), format!("{kw}|{cc}")));
    if bearer(&headers) != e.valid_token {
        return unauthorized();
    }
    let words: Vec<String> = kw.to_lowercase().split_whitespace().map(str::to_string).collect();
    let els: Vec<Value> = e.offers.iter().filter(|o| words.iter().any(|w| o.title.to_lowercase().contains(w.as_str()))).map(|o| epic_el(o, &cc)).collect();
    axum::Json(json!({"data": {"Catalog": {"searchStore": {"elements": els}}}})).into_response()
}

async fn epic_price(State(s): S, headers: HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    s.hit("price-engine");
    let mut e = s.epic.lock().unwrap();
    let ids: Vec<String> = body["lineOffers"].as_array().unwrap().iter().map(|l| l["offerId"].as_str().unwrap().to_string()).collect();
    e.log.push(("price".into(), bearer(&headers), format!("{}|{}|{}", ids.join(","), body["country"], body["accountId"])));
    if bearer(&headers) != e.valid_token {
        return unauthorized();
    }
    if !e.price_engine_ok {
        return StatusCode::NOT_FOUND.into_response();
    }
    let lines: Vec<Value> = e
        .offers
        .iter()
        .filter(|o| ids.contains(&o.id))
        .map(|o| json!({"offerId": o.id, "price": {"currencyCode": "USD", "originalPrice": o.original, "discountPrice": o.now, "discountPercentage": 100}}))
        .collect();
    axum::Json(json!({"country": body["country"], "namespace": "ns", "totalPaymentPrice": {"paymentCurrencyCode": "USD", "paymentCurrencyDecimal": 2}, "lineOffers": lines})).into_response()
}

async fn epic_catalog(State(s): S, headers: HeaderMap, axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    s.hit("catalog");
    let mut e = s.epic.lock().unwrap();
    let q = q.unwrap_or_default();
    let ids: Vec<String> = q.split('&').filter_map(|kv| kv.strip_prefix("id=")).map(str::to_string).collect();
    e.log.push(("catalog".into(), bearer(&headers), ids.join(",")));
    if bearer(&headers) != e.valid_token {
        return unauthorized();
    }
    let m: serde_json::Map<String, Value> = e
        .offers
        .iter()
        .filter(|o| ids.contains(&o.id))
        .map(|o| (o.id.clone(), json!({"title": o.title, "namespace": o.ns, "currencyCode": "USD", "currencyDecimals": 2, "currentPrice": o.now, "price": o.original, "basePrice": o.original})))
        .collect();
    axum::Json(Value::Object(m)).into_response()
}

async fn epic_billing(State(s): S, headers: HeaderMap, Path(acct): Path<String>) -> Response {
    s.hit("billing");
    let mut e = s.epic.lock().unwrap();
    e.log.push(("billing".into(), bearer(&headers), acct));
    if bearer(&headers) != e.valid_token {
        return unauthorized();
    }
    axum::Json(json!({"billingAccountName": "x", "country": e.billing_country, "countrySource": "ACCOUNT", "currency": "USD"})).into_response()
}

/// The app's wrapper of `EpicAuth`, as a fake: the token it hands out, and what a refresh gives.
pub struct TestSession {
    pub token: Mutex<String>,
    pub after_refresh: Mutex<String>,
    pub refreshes: AtomicU32,
    pub signed_in: std::sync::atomic::AtomicBool,
}

impl TestSession {
    pub fn new(token: &str, after_refresh: &str) -> Arc<TestSession> {
        Arc::new(TestSession { token: Mutex::new(token.into()), after_refresh: Mutex::new(after_refresh.into()), refreshes: AtomicU32::new(0), signed_in: std::sync::atomic::AtomicBool::new(true) })
    }
    fn current(&self) -> EpicSession {
        EpicSession { account_id: "acct123".into(), token: ApiKey::new(self.token.lock().unwrap().clone()) }
    }
}

impl EpicSessionProvider for TestSession {
    fn available(&self) -> bool {
        self.signed_in.load(Ordering::SeqCst)
    }
    fn session(&self) -> futures_util::future::BoxFuture<'_, grapple_store::Result<EpicSession>> {
        Box::pin(async move { Ok(self.current()) })
    }
    fn refresh(&self) -> futures_util::future::BoxFuture<'_, grapple_store::Result<EpicSession>> {
        Box::pin(async move {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            let new = self.after_refresh.lock().unwrap().clone();
            *self.token.lock().unwrap() = new;
            Ok(self.current())
        })
    }
}
