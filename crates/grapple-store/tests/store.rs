//! The store service against a fake Steam/Epic/FX/ITAD on a local port. No live network.

mod common;

use common::*;
use grapple_store::{Friend, SimilarKind, StoreFilters, StoreKind, StoreSettings, StoreEvent};
use std::time::{Duration, Instant};

const IDLE: Duration = Duration::from_secs(20);

fn survival(id: u32, name: &str) -> G {
    game(id, name).genres(&["Action", "Survival"]).tags(&[("Survival", 100), ("Crafting", 90), ("Open World", 80)])
}

/// The standard world: a top list with one of everything the filters care about.
fn world() -> World {
    let mut w = world_with(vec![
        survival(1, "Survival Craft").price(20000, 10000),
        game(2, "Co-op Fun").genres(&["Action"]).tags(&[("Co-op", 100)]).price(5000, 5000),
        game(3, "Maid Cafe").tags(&[("Sexual Content", 83), ("Hentai", 70), ("Action", 10)]).price(6000, 6000),
        game(4, "Mature Game").desc(&[1, 5]).price(6000, 6000),
        game(5, "Gore Fest").desc(&[2, 5]).price(6000, 6000),
        game(6, "Owned Game").price(6000, 6000),
        game(7, "Epic Owned Title").price(6000, 6000),
        game(8, "Soon Game").soon(),
        game(9, "Plain New").price(3000, 3000),
        game(10, "Cheap A").price(4000, 4000),
        game(11, "Cheap B").price(2500, 1000),
        game(12, "Cheap Adult").tags(&[("Sexual Content", 90)]).price(3000, 3000),
        game(13, "DLC Thing").kind("dlc").price(1000, 1000),
        game(14, "Puzzle Pal").genres(&["Puzzle"]).tags(&[("Puzzle", 100)]).price(7000, 7000),
        game(15, "New Adult").desc(&[3]).price(3000, 3000),
        // the library's games
        survival(100, "Valheim Like").price(9000, 9000),
        survival(101, "Rust Like").price(9000, 9000),
    ]);
    w.top = vec![1, 2, 3, 4, 5, 6, 7, 13, 14];
    w.new = vec![8, 9, 15];
    w.under = vec![10, 11, 12];
    w
}

async fn built(w: World) -> Harness {
    let h = harness(w).await;
    {
        let mut o = h.lib.owned.lock().unwrap();
        o.push(owned("steam:6", "Owned Game", 600));
        o.push(owned("epic:Sugar", "Epic Owned Title", 600));
        o.push(owned("steam:100", "Valheim Like", 89 * 60));
        o.push(owned("steam:101", "Rust Like", 300 * 60));
    }
    h.store.refresh_now(false).await.unwrap();
    h
}

fn keys(g: &[grapple_store::StoreGame]) -> Vec<String> {
    g.iter().map(|g| g.key.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn front_hides_owned_adult_and_unreleased_and_counts_what_the_filter_hid() {
    let h = built(world()).await;
    let f = h.store.front().await.unwrap();
    let top = keys(&f.top);
    assert!(top.contains(&"steam:1".to_string()) && top.contains(&"steam:2".to_string()) && top.contains(&"steam:14".to_string()));
    assert!(!top.contains(&"steam:3".to_string()), "adult by community tag only (no descriptor)");
    assert!(!top.contains(&"steam:4".to_string()), "adult by descriptor");
    assert!(top.contains(&"steam:5".to_string()), "violent is shown while that filter is off");
    assert!(!top.contains(&"steam:6".to_string()), "owned on Steam");
    assert!(!top.contains(&"steam:7".to_string()), "owned on Epic (same title)");
    assert!(!top.contains(&"steam:13".to_string()), "not a game");
    assert_eq!(keys(&f.under.games), ["steam:10", "steam:11"]);
    assert!(!keys(&f.under.games).contains(&"steam:12".to_string()));
    assert_eq!(f.under.hidden, 1, "Cheap Adult");
    assert_eq!(keys(&f.new_releases.games), ["steam:9"], "the unreleased one is not shown");
    assert_eq!(f.new_releases.hidden, 1, "New Adult");
    assert!(f.under.tier_minor > 0);
    assert_eq!(f.genres.len(), 6);
    assert!(f.genres[0].art.starts_with("grapple://store/steam%3A"));

    // hide violent: Gore Fest goes
    let mut s = h.store.settings_get();
    s.hide_violent = true;
    h.store.settings_set(s.clone()).unwrap();
    let f = h.store.front().await.unwrap();
    assert!(!keys(&f.top).contains(&"steam:5".to_string()));
    // adult filter off: the adult ones return and nothing is counted as hidden
    s.hide_adult = false;
    h.store.settings_set(s).unwrap();
    let f = h.store.front().await.unwrap();
    assert!(keys(&f.top).contains(&"steam:3".to_string()) && keys(&f.top).contains(&"steam:4".to_string()));
    assert_eq!(f.new_releases.hidden, 0);
    // the tag page was fetched with the age-gate cookies every time
    assert_eq!(h.srv.tag_requests_without_cookie.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn games_the_filter_cannot_judge_yet_are_held_back() {
    let h = harness(world()).await;
    let db = h.store.db();
    // lists cached, details cached, tags not yet
    db.kv_set("top:ZA", &vec![grapple_store::steam::ListItem { appid: 3, name: "Maid Cafe".into(), price: None, discount_expires: None }, grapple_store::steam::ListItem { appid: 2, name: "Co-op Fun".into(), price: None, discount_expires: None }]).unwrap();
    for id in [2, 3] {
        let g = h.srv.world.lock().unwrap().games[&id].clone();
        db.put_info(
            "ZA",
            &grapple_store::steam::AppInfo {
                appid: id,
                name: g.name.clone(),
                kind: "game".into(),
                is_free: false,
                short: String::new(),
                developers: vec![],
                publishers: vec![],
                release: None,
                coming_soon: false,
                genres: vec![],
                descriptors: vec![],
                reviews: None,
                shots: vec![],
                price: None,
                windows: true,
                linux: true,
            },
        )
        .unwrap();
    }
    let f = h.store.front().await.unwrap();
    assert!(f.top.is_empty(), "no tags, adult filter on: nothing is shown that could be adult");
    // once a refresh has fetched the tags, the clean one shows and the tagged one does not
    h.store.refresh_now(false).await.unwrap();
    let f = h.store.front().await.unwrap();
    let k = keys(&f.top);
    assert!(k.contains(&"steam:2".to_string()) && !k.contains(&"steam:3".to_string()), "{k:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_front_answers_from_cache_without_the_network() {
    let h = built(world()).await;
    let before = h.srv.steam_total();
    assert!(before > 0);
    let t = Instant::now();
    for _ in 0..20 {
        let f = h.store.front().await.unwrap();
        assert!(!f.top.is_empty());
    }
    let per = t.elapsed() / 20;
    assert!(per < Duration::from_millis(60), "front took {per:?} per call");
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.srv.steam_total(), before, "a fresh cache means no request");
    assert_eq!(h.store.request_counts().steam, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_front_is_empty_at_once_then_fills_and_announces_it() {
    let h = harness(world()).await;
    let t = Instant::now();
    let f = h.store.front().await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(f.top.is_empty() && f.picked.is_empty() && f.updated_at == 0, "nothing cached yet");
    assert!(h.store.wait_idle(IDLE).await);
    assert!(h.changed_count() > 0, "store-changed was raised");
    let f = h.store.front().await.unwrap();
    assert!(!f.top.is_empty());
    assert!(!f.epic_free.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_build_request_budget() {
    let mut w = world();
    // 40 more games on each list
    for i in 0..120u32 {
        let id = 1000 + i;
        let g = game(id, &format!("Filler {i}")).price(10000, if i % 7 == 0 { 7000 } else { 10000 });
        w.games.insert(id, g);
        match i % 3 {
            0 => w.top.push(id),
            1 => w.new.push(id),
            _ => w.under.push(id),
        }
    }
    w.wishlist = vec![1000, 1001, 1002, 1, 2];
    let h = harness(w).await;
    h.lib.owned.lock().unwrap().extend((0..12).map(|i| owned(&format!("steam:{}", 100 + i % 2), "x", 60)));
    h.store.refresh_now(false).await.unwrap();
    let c = h.srv.counts.lock().unwrap().clone();
    let total = h.srv.steam_total();
    println!("cold build against the fake: {total} Steam requests: {:?}; by kind {:?}", c, h.store.request_kinds());
    assert!(total <= 130, "a cold build must stay well under Steam's 200 per 5 minutes: {total}");
    assert!(h.srv.count("tags") <= 60);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_429_stops_the_run_and_the_next_run_finishes() {
    let h = harness(world()).await;
    h.srv.limit_next.store(1, std::sync::atomic::Ordering::SeqCst);
    let r = h.store.refresh_now(false).await;
    assert!(matches!(r, Err(grapple_store::Error::RateLimited)), "{r:?}");
    let after_429 = h.srv.steam_total();
    // the limiter now backs off: a new run waits for it instead of hammering
    let t = Instant::now();
    h.store.refresh_now(false).await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(150), "waited out the backoff: {:?}", t.elapsed());
    assert!(h.srv.steam_total() > after_429);
    let f = h.store.front().await.unwrap();
    assert!(!f.top.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prices_are_converted_to_the_display_currency_and_native_is_kept() {
    let h = built(world()).await;
    let f = h.store.front().await.unwrap();
    let g = f.top.iter().find(|g| g.key == "steam:1").unwrap();
    let p = &g.prices[0];
    assert_eq!(p.store, StoreKind::Steam);
    assert_eq!((p.final_price.minor, p.final_price.currency.as_str()), (10000, "ZAR"), "default display = Steam's currency");
    assert_eq!((p.original.minor, p.discount_pct), (20000, 50));
    assert_eq!(p.url, "https://store.steampowered.com/app/1/");

    let mut s = h.store.settings_get();
    assert_eq!(s.display_currency, "ZAR");
    s.display_currency = "USD".into();
    h.store.settings_set(s.clone()).unwrap();
    let f = h.store.front().await.unwrap();
    let p = &f.top.iter().find(|g| g.key == "steam:1").unwrap().prices[0];
    assert_eq!((p.final_price.minor, p.final_price.currency.as_str()), (625, "USD"), "100 ZAR at 16 per dollar");
    assert_eq!((p.native.minor, p.native.currency.as_str()), (10000, "ZAR"), "checkout keeps the store's own price");

    s.display_currency = "XYZ".into();
    assert!(h.store.settings_set(s).is_err());
    let cur = h.store.currencies();
    assert!(cur.iter().any(|c| c.code == "ZAR" && c.name == "South African rand" && c.decimals == 2));
    assert!(cur.iter().any(|c| c.code == "JPY" && c.decimals == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_steam_country_comes_from_steams_files_a_setting_or_the_environment() {
    let h = harness(world()).await;
    assert_eq!(h.store.region().source, "steam-config", "what Steam's files said at start");
    *h.lib.region.lock().unwrap() = None;
    h.store.redetect_region();
    assert_eq!(h.store.region().source, "default");
    assert_eq!(h.store.region().currency, "USD");
    let r = h.store.region_set(Some("za".into())).unwrap();
    assert_eq!((r.country.as_str(), r.currency.as_str(), r.source.as_str()), ("ZA", "ZAR", "setting"));
    assert!(h.store.region_set(Some("zzz".into())).is_err());
    let r = h.store.region_set(None).unwrap();
    assert_eq!(r.source, "default");
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wishlist_merges_steam_and_local_and_alerts_once_when_a_game_goes_on_sale() {
    let mut w = world();
    w.wishlist = vec![1, 2];
    let h = harness(w).await;
    h.store.wishlist_tick_now().await;
    let wl = h.store.wishlist().await.unwrap();
    h.store.wait_idle(IDLE).await;
    assert_eq!(wl.len(), 2);
    // Survival Craft is on sale (50%) from the start: the first sync alerts about it once
    let first = h.sales();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!((first[0].key.as_str(), first[0].discount_pct, first[0].store), ("steam:1", 50, StoreKind::Steam));
    assert_eq!(first[0].name, "Survival Craft");
    assert_eq!(first[0].final_price.minor, 10000);

    // no news, no second alert
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 1);

    // Co-op Fun goes on sale
    h.srv.world.lock().unwrap().games.get_mut(&2).unwrap().fin = 2500;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    let s = h.sales();
    assert_eq!(s.len(), 2);
    assert_eq!((s[1].key.as_str(), s[1].discount_pct), ("steam:2", 50));

    // it ends, then starts again: alerted again
    h.srv.world.lock().unwrap().games.get_mut(&2).unwrap().fin = 5000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 2);
    h.srv.world.lock().unwrap().games.get_mut(&2).unwrap().fin = 2000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 3);

    // the per-game switch and the global one
    h.store.notify_set("steam:2", false).await.unwrap();
    h.srv.world.lock().unwrap().games.get_mut(&2).unwrap().fin = 5000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    h.srv.world.lock().unwrap().games.get_mut(&2).unwrap().fin = 1000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 3, "switched off for this game");
    let g = h.store.wishlist().await.unwrap().into_iter().find(|g| g.key == "steam:2").unwrap();
    assert!(g.wishlisted && !g.notify_on_sale);
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_wishlist_on_sale_at_once_does_not_become_a_wall_of_alerts() {
    let mut w = world_with((1..=6).map(|i| game(i, &format!("Sale {i}")).price(10000, 5000)).collect());
    w.wishlist = (1..=6).collect();
    let h = harness(w).await;
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 3);
    let mut s = h.store.settings_get();
    assert!(s.wishlist_alerts);
    s.wishlist_alerts = false;
    h.store.settings_set(s).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(h.sales().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wishlist_can_be_edited_locally_and_never_writes_to_steam() {
    let mut w = world();
    w.wishlist = vec![2];
    let h = harness(w).await;
    h.store.wishlist_tick_now().await;
    h.store.wishlist_set("steam:14", true).await.unwrap();
    h.store.wait_idle(IDLE).await;
    let wl = h.store.wishlist().await.unwrap();
    h.store.wait_idle(IDLE).await;
    assert_eq!(keys(&wl).len(), 2);
    assert!(wl.iter().all(|g| g.wishlisted && g.notify_on_sale));
    // removing a Steam-synced one hides it here; a later sync does not bring it back
    h.store.wishlist_set("steam:2", false).await.unwrap();
    h.store.db().kv_touch("wishlist_sync", 0).unwrap();
    h.store.wishlist_tick_now().await;
    let wl = h.store.wishlist().await.unwrap();
    assert_eq!(keys(&wl), ["steam:14"]);
    assert!(h.store.wishlist_set("../etc", true).await.is_err());
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_free_games_are_real_and_epic_prices_need_a_source() {
    let h = built(world()).await;
    let f = h.store.front().await.unwrap();
    let names: Vec<&str> = f.epic_free.iter().map(|e| e.game.name.as_str()).collect();
    assert_eq!(names, ["Bad Cheese", "Sker Ritual"], "free now first, then free next");
    assert!(f.epic_free[0].now && !f.epic_free[1].now);
    assert_eq!(f.epic_free[0].until.as_deref(), Some("2026-10-22T15:00:00.000Z"));
    let bad = &f.epic_free[0].game.prices[0];
    assert_eq!((bad.store, bad.final_price.minor, bad.discount_pct), (StoreKind::Epic, 0, 100));
    assert!(bad.original.minor > 0 && bad.url.starts_with("https://store.epicgames.com/"));
    assert!(f.epic_free[0].game.art.cover.as_deref().unwrap().starts_with("grapple://store/epic%3A"));

    // no key: a Steam game has only its Steam price, never an invented Epic one
    let g = h.store.game("steam:1").await.unwrap();
    assert_eq!(g.game.prices.len(), 1);
    assert!(!h.store.settings_get().itad_key.unwrap());
    assert_eq!(h.store.epic_source_name(), "none");
    assert!(h.store.buy_target("steam:1", StoreKind::Epic).is_err());

    // with a key (kept in the key store, header-only) Epic's price shows and Buy has a target
    *h.srv.itad_epic_price.lock().unwrap() = Some((1500, 500, 67)); // dollars: $15.00 -> $5.00
    let s = h.store.itad_key_set(Some("test-key-123".into())).await.unwrap();
    assert_eq!(s.itad_key, Some(true));
    assert_eq!(h.store.epic_source_name(), "isthereanydeal");
    h.store.wait_idle(IDLE).await;
    h.store.refresh_now(false).await.unwrap();
    let g = h.store.game("steam:1").await.unwrap().game;
    assert_eq!(g.prices.len(), 2, "{:?}", g.prices);
    let e = g.prices.iter().find(|p| p.store == StoreKind::Epic).unwrap();
    assert_eq!((e.native.minor, e.native.currency.as_str(), e.discount_pct), (500, "USD", 67));
    assert_eq!(e.final_price.currency, "ZAR");
    assert_eq!(e.final_price.minor, 8000, "5 dollars at 16 rand");
    assert_eq!(g.cheapest, Some(StoreKind::Epic), "80 against 100 rand");
    let t = h.store.buy_target("steam:1", StoreKind::Epic).unwrap();
    assert!(t.url.starts_with("https://itad.example/link/"));
    // the key travelled in a header, never in a URL or the query
    let log = h.srv.itad_log.lock().unwrap().clone();
    assert!(!log.is_empty() && log.iter().all(|(_, header, q)| *header && !q.contains("test-key")));
    // forgetting the key forgets the Epic prices source
    let s = h.store.itad_key_set(None).await.unwrap();
    assert_eq!(s.itad_key, Some(false));
    assert_eq!(h.store.epic_source_name(), "none");
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_owned_games_and_epic_wishlist() {
    let h = harness(world()).await;
    h.lib.owned.lock().unwrap().push({
        let mut o = owned("epic:Sugar", "Bad Cheese", 10);
        o.epic_namespace = Some("a0c80bf673a548bc97b4bb25abfaf2b4".into());
        o
    });
    h.store.refresh_now(false).await.unwrap();
    let f = h.store.front().await.unwrap();
    let names: Vec<&str> = f.epic_free.iter().map(|e| e.game.name.as_str()).collect();
    assert_eq!(names, ["Sker Ritual"], "the Epic game Josh owns is not offered");
    // wishlist an Epic game; its sale (free now) alerts
    let key = f.epic_free[0].game.key.clone();
    h.store.wishlist_set(&key, true).await.unwrap();
    h.store.wishlist_tick_now().await;
    let wl = h.store.wishlist().await.unwrap();
    assert_eq!(keys(&wl), [key.clone()]);
    let d = h.store.game(&key).await.unwrap();
    assert!(d.game.wishlisted && d.more_like_this.is_empty());
    let t = h.store.buy_target(&key, StoreKind::Epic).unwrap();
    assert_eq!(t.namespace.as_deref(), Some(key.split(':').nth(1).unwrap()));
    assert_eq!(t.offer_id.as_deref(), Some(key.split(':').nth(2).unwrap()));
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picks_follow_taste_and_say_why() {
    let h = built(world()).await;
    let f = h.store.front().await.unwrap();
    assert!(!f.featured.is_empty());
    assert_eq!(f.featured[0].key, "steam:1", "the survival game for a survival library");
    let sim: Vec<&str> = f.featured[0].similar.iter().map(|s| s.id.as_str()).collect();
    assert!(sim.contains(&"steam:100") && sim.contains(&"steam:101"), "{sim:?}");
    assert!(f.featured[0].similar.iter().all(|s| s.kind == SimilarKind::Game && s.icon.as_deref().is_some_and(|i| i.starts_with("grapple://art/steam:"))));
    // never owned or filtered ones in the picks
    for g in f.featured.iter().chain(&f.picked) {
        assert!(!["steam:6", "steam:7", "steam:3", "steam:4", "steam:13"].contains(&g.key.as_str()), "{}", g.key);
    }
    // the puzzle game, far from the taste, ranks below the survival one
    let all: Vec<String> = f.featured.iter().chain(&f.picked).map(|g| g.key.clone()).collect();
    if let (Some(a), Some(b)) = (all.iter().position(|k| k == "steam:1"), all.iter().position(|k| k == "steam:14")) {
        assert!(a < b);
    }
    // Home: the top 5
    let home = h.store.home_picks().await.unwrap();
    assert!(!home.is_empty() && home.len() <= 5);
    assert_eq!(home[0].key, "steam:1");
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn friends_who_play_a_game_are_the_reason() {
    let h = harness(world()).await;
    h.lib.owned.lock().unwrap().push(owned("steam:100", "Valheim Like", 600));
    h.lib.friends.lock().unwrap().push(Friend { id: "f1".into(), name: "rowan".into(), avatar: Some("grapple://avatar/f1".into()), playing_appid: Some(2), playing_name: Some("Co-op Fun".into()) });
    h.store.refresh_now(false).await.unwrap();
    let f = h.store.front().await.unwrap();
    let g = f.featured.iter().chain(&f.picked).find(|g| g.key == "steam:2").expect("the friend's game is picked");
    assert_eq!(g.similar.len(), 1);
    assert_eq!((g.similar[0].kind, g.similar[0].name.as_str(), g.similar[0].icon.as_deref()), (SimilarKind::Friend, "rowan", Some("grapple://avatar/f1")));
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_page_has_prices_shots_lowest_seen_and_more_like_this() {
    let h = built(world()).await;
    let d = h.store.game("steam:1").await.unwrap();
    let g = &d.game;
    assert_eq!((g.name.as_str(), g.short.as_str()), ("Survival Craft", "About Survival Craft."));
    // genres are the Steam genres among the community tags (GetItems has no genre list)
    assert!(g.genres == ["Action"] && g.tags.contains(&"Crafting".to_string()) && g.tags.contains(&"Survival".to_string()), "{:?} {:?}", g.genres, g.tags);
    assert_eq!(g.art.shots, ["grapple://store/steam%3A1/shot0"]);
    assert_eq!(g.reviews, Some(5000));
    assert_eq!(g.release.as_deref(), Some("1 Jan, 2025"));
    assert_eq!(g.lowest_seen.as_ref().unwrap().minor, 10000);
    assert!(d.more_like_this.iter().all(|m| m.key != "steam:1"));
    assert!(!d.more_like_this.is_empty());
    assert!(h.store.game("steam:999999").await.is_err());
    assert!(h.store.game("steam:abc").await.is_err());

    // the price falls, then rises: the lowest grapple has seen stays
    h.srv.world.lock().unwrap().games.get_mut(&1).unwrap().fin = 6000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.refresh_now(false).await.unwrap();
    h.srv.world.lock().unwrap().games.get_mut(&1).unwrap().fin = 20000;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.refresh_now(false).await.unwrap();
    let g = h.store.game("steam:1").await.unwrap().game;
    assert_eq!(g.prices[0].final_price.minor, 20000);
    assert_eq!(g.lowest_seen.unwrap().minor, 6000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_filters_and_hides() {
    let h = built(world()).await;
    let r = h.store.search("craft", StoreFilters::default()).await.unwrap();
    assert_eq!(keys(&r), ["steam:1"]);
    assert!(h.store.search("", StoreFilters::default()).await.unwrap().is_empty());
    // adult results are hidden until asked for
    let r = h.store.search("maid", StoreFilters::default()).await.unwrap();
    assert!(r.is_empty());
    let r = h.store.search("maid", StoreFilters { include_hidden: Some(true), ..Default::default() }).await.unwrap();
    assert_eq!(keys(&r), ["steam:3"]);
    // owned never shows
    assert!(h.store.search("owned game", StoreFilters::default()).await.unwrap().is_empty());
    // genre browsing goes through Steam's tag search
    let r = h.store.search("", StoreFilters { genre: Some("Survival".into()), ..Default::default() }).await.unwrap();
    assert!(keys(&r).contains(&"steam:1".to_string()) && !keys(&r).contains(&"steam:14".to_string()), "{:?}", keys(&r));
    // price and sale filters
    let r = h.store.search("e", StoreFilters { max_price_minor: Some(6000), ..Default::default() }).await.unwrap();
    assert!(r.iter().all(|g| g.prices.iter().all(|p| p.final_price.minor <= 6000)));
    let r = h.store.search("craft", StoreFilters { on_sale: Some(true), ..Default::default() }).await.unwrap();
    assert_eq!(keys(&r), ["steam:1"]);
    let r = h.store.search("fun", StoreFilters { on_sale: Some(true), ..Default::default() }).await.unwrap();
    assert!(r.is_empty());
    // Epic offers match by title
    let r = h.store.search("cheese", StoreFilters::default()).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].prices[0].store, StoreKind::Epic);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_tiers_load_on_demand_and_are_cached() {
    let h = built(world()).await;
    let a = h.store.under(5000, false).await.unwrap();
    assert_eq!(a.tier_minor, 5000);
    assert!(a.games.iter().all(|g| g.prices[0].final_price.minor <= 5000));
    let searches = h.srv.count("search");
    let b = h.store.under(5000, false).await.unwrap();
    assert_eq!(keys(&a.games), keys(&b.games));
    assert_eq!(h.srv.count("search"), searches, "cached for the hour");
    let hidden = h.store.under(5000, true).await.unwrap();
    assert!(hidden.games.len() >= a.games.len());
    assert!(h.store.under(0, false).await.is_err());
    // the tier is remembered as the store's tier
    assert_eq!(h.store.front().await.unwrap().under.tier_minor, 5000);
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn art_is_fetched_once_validated_and_falls_back() {
    let w = world_with(vec![game(1, "A"), game(2, "B").no_cover()]);
    let h = harness(w).await;
    h.store.db().put_info_missing("ZA", 99).unwrap();
    // never fetched with a bad key or kind, and no request is made for them
    for (k, kind) in [("steam:../x", "cover"), ("steam:1", "icon"), ("epic:x:y", "cover"), ("other:1", "cover"), ("steam:1", "shot99")] {
        assert!(h.store.art(k, kind).await.is_err(), "{k} {kind}");
    }
    assert_eq!(h.srv.count("cdn"), 0);

    let a = h.store.art("steam:1", "cover").await.unwrap().unwrap();
    assert_eq!(a.mime, "image/jpeg");
    assert!(a.path.starts_with(h.dir.path().join("art")));
    let again = h.store.art("steam:1", "cover").await.unwrap().unwrap();
    assert_eq!(a.path, again.path);
    assert_eq!(h.srv.count("cdn:library_600x900.jpg"), 1, "once, then from disk");
    // a game without a portrait cover gets its header
    let b = h.store.art("steam:2", "cover").await.unwrap().unwrap();
    assert_eq!(b.mime, "image/jpeg");
    assert_eq!(h.srv.count("cdn:header.jpg"), 1);
    // logo is a PNG; a missing picture is remembered as missing
    assert_eq!(h.store.art("steam:1", "logo").await.unwrap().unwrap().mime, "image/png");
    assert!(h.store.art("steam:3", "cover").await.unwrap().is_none());
    let n = h.srv.count("cdn");
    assert!(h.store.art("steam:3", "cover").await.unwrap().is_none());
    assert_eq!(h.srv.count("cdn"), n, "a miss is not asked again at once");
    // screenshots need the details first
    assert!(h.store.art("steam:1", "shot0").await.unwrap().is_none());
    h.store.game("steam:1").await.unwrap();
    assert_eq!(h.store.art("steam:1", "shot0").await.unwrap().unwrap().mime, "image/jpeg");
    // a quiet disk cache: only the validated names exist
    let names: Vec<String> = std::fs::read_dir(h.dir.path().join("art")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| n.starts_with("steam-") && !n.contains("..") && !n.contains('/')), "{names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_round_trip_over_the_wire_shape() {
    let h = harness(world()).await;
    let s = h.store.settings_get();
    assert_eq!(s, StoreSettings { display_currency: "ZAR".into(), hide_adult: true, hide_violent: false, stores: grapple_store::StoresOn { steam: true, epic: true }, wishlist_alerts: true, itad_key: Some(false) });
    let v = serde_json::to_value(&s).unwrap();
    let back: StoreSettings = serde_json::from_value(v).unwrap();
    let saved = h.store.settings_set(back).unwrap();
    assert_eq!(saved.display_currency, "ZAR");
    // the stores switch removes a store's games and prices
    let mut s = saved;
    s.stores.epic = false;
    h.store.settings_set(s).unwrap();
    h.store.refresh_now(false).await.unwrap();
    assert!(h.store.front().await.unwrap().epic_free.is_empty());
    let events = h.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, StoreEvent::Changed(c) if c.sections.iter().any(|s| s == "settings"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reading_a_fresh_cache_raises_no_events_so_the_ui_cannot_loop() {
    let mut w = world();
    w.wishlist = vec![2];
    let h = built(w).await;
    h.store.wishlist_tick_now().await;
    let _ = h.store.wishlist().await.unwrap();
    h.store.wait_idle(IDLE).await;
    let _ = h.store.wishlist().await.unwrap();
    h.store.wait_idle(IDLE).await;
    h.events.lock().unwrap().clear();
    let before = h.srv.steam_total();
    for _ in 0..3 {
        let _ = h.store.front().await.unwrap();
        let _ = h.store.wishlist().await.unwrap();
        let _ = h.store.home_picks().await.unwrap();
        let _ = h.store.under(h.store.front().await.unwrap().under.tier_minor, false).await.unwrap();
    }
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.changed_count(), 0, "{:?}", h.events.lock().unwrap());
    assert_eq!(h.srv.steam_total(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_refresh_does_not_retry_in_a_loop() {
    let h = harness(world()).await;
    h.srv.limit_next.store(1_000_000, std::sync::atomic::Ordering::SeqCst); // Steam says 429 to everything
    let _ = h.store.front().await.unwrap();
    h.store.wait_idle(IDLE).await;
    let after_first = h.srv.steam_total();
    for _ in 0..5 {
        let _ = h.store.front().await.unwrap();
    }
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.srv.steam_total(), after_first, "front() does not start another refresh within two minutes of the last");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_details_bring_the_tags_so_no_per_game_request_is_needed() {
    let h = built(world()).await;
    assert!(h.srv.count("items") >= 1 && h.srv.count("taglist") == 1);
    assert_eq!(h.srv.count("appdetails"), 0, "the bulk answer covers every game");
    assert_eq!(h.srv.count("tags"), 0, "and so does its tag list");
    // the filters work from the bulk data alone: adult by community tag, adult by descriptor, violent kept
    let top = keys(&h.store.front().await.unwrap().top);
    assert!(top.contains(&"steam:1".to_string()) && top.contains(&"steam:5".to_string()));
    assert!(!top.contains(&"steam:3".to_string()) && !top.contains(&"steam:4".to_string()));
    // the name list is kept for the week
    h.store.refresh_now(true).await.unwrap();
    assert_eq!(h.srv.count("taglist"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_bulk_service_each_game_is_asked_for_as_before() {
    let h = harness(world()).await;
    h.srv.no_items.store(true, std::sync::atomic::Ordering::SeqCst);
    h.store.refresh_now(false).await.unwrap();
    assert!(h.srv.count("appdetails") > 0 && h.srv.count("tags") > 0);
    let top = keys(&h.store.front().await.unwrap().top);
    assert!(top.contains(&"steam:1".to_string()) && !top.contains(&"steam:3".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tags_the_bulk_answer_cannot_name_come_from_the_store_page() {
    let h = harness(world()).await;
    // "Hentai" has no name in the tag list: the games that carry it need the page, the others do not
    *h.srv.unlisted_tag.lock().unwrap() = Some("Hentai");
    h.store.refresh_now(false).await.unwrap();
    let pages = h.srv.count("tags");
    assert!(pages >= 1, "Maid Cafe is read from its page");
    assert_eq!(pages, 1, "only the one game the bulk answer cannot name");
    assert_eq!(h.srv.count("appdetails"), 0);
    let top = keys(&h.store.front().await.unwrap().top);
    assert!(!top.contains(&"steam:3".to_string()), "still judged adult, now from the page's tags");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_releases_show_their_hidden_games_on_request() {
    let h = built(world()).await;
    let a = h.store.new_releases(false).await.unwrap();
    assert_eq!((keys(&a.games), a.hidden), (vec!["steam:9".to_string()], 1));
    let all = h.store.new_releases(true).await.unwrap();
    assert!(keys(&all.games).contains(&"steam:15".to_string()) && keys(&all.games).contains(&"steam:9".to_string()));
    assert_eq!(all.hidden, 0, "nothing is left hidden once shown");
    // the setting stays as it was, and the front still counts them
    assert!(h.store.settings_get().hide_adult);
    assert_eq!(h.store.front().await.unwrap().new_releases.hidden, 1);
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steam_wishlist_games_are_hidden_here_and_can_be_shown_again() {
    let mut w = world();
    w.wishlist = vec![2, 14];
    let h = harness(w).await;
    h.store.wishlist_tick_now().await;
    assert!(h.store.wishlist_hidden().await.unwrap().is_empty());
    h.store.wishlist_set("steam:2", false).await.unwrap();
    assert_eq!(keys(&h.store.wishlist().await.unwrap()), ["steam:14"]);
    let hidden = h.store.wishlist_hidden().await.unwrap();
    assert_eq!(hidden.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["steam:2"]);
    // a sync does not bring it back; nothing is written to Steam (the fake has no write route to hit)
    h.store.db().kv_touch("wishlist_sync", 0).unwrap();
    h.store.wishlist_tick_now().await;
    assert_eq!(keys(&h.store.wishlist().await.unwrap()), ["steam:14"]);
    assert_eq!(h.store.wishlist_unhide().await.unwrap(), 1);
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.store.wishlist_unhide().await.unwrap(), 0);
    let mut back = keys(&h.store.wishlist().await.unwrap());
    back.sort();
    assert_eq!(back, ["steam:14", "steam:2"]);
    h.store.wait_idle(IDLE).await;
}
