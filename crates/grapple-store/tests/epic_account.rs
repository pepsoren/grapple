//! Epic prices with the Epic login, against a local mock built from the shapes in docs/epic-prices.md.
//! Nothing here touches Epic or any real token.

mod common;

use common::*;
use grapple_store::{StoreKind, epic::EpicPriceSource};
use std::sync::atomic::Ordering;
use std::time::Duration;

const IDLE: Duration = Duration::from_secs(20);

fn world() -> World {
    let mut w = world_with(vec![
        game(1, "Survival Craft").price(20000, 20000),
        game(2, "Co-op Fun").price(5000, 5000),
        game(3, "Twin Titles").price(5000, 5000),
        game(4, "Unlisted").price(5000, 5000),
        game(5, "Lookalike").price(5000, 5000),
    ]);
    w.top = vec![1, 2, 3, 4, 5];
    w
}

/// The fake Epic catalog: one confident match, one wrong-maker namesake, an ambiguous pair, an edition.
fn catalog(h: &Harness) {
    let mut e = h.srv.epic.lock().unwrap();
    e.offers = vec![
        eoffer("ns1", "offer-survival", "Survival Craft", &["Dev"], 2000, 1000),
        eoffer("ns2", "offer-coop", "Co-op Fun", &["Some Other Studio"], 1000, 1000),
        eoffer("ns3", "offer-twin-a", "Twin Titles", &["Dev"], 1000, 1000),
        eoffer("ns3", "offer-twin-b", "Twin Titles", &["Dev"], 1500, 1500),
        eoffer("ns5", "offer-lookalike-deluxe", "Lookalike Deluxe Edition", &["Dev"], 3000, 3000),
    ];
    e.offers[3].offer_type = "BASE_GAME";
    e.offers[2].offer_type = "BASE_GAME";
}

async fn with_account(token: &str, after: &str) -> (Harness, std::sync::Arc<TestSession>) {
    let h = harness(world()).await;
    catalog(&h);
    let session = TestSession::new(token, after);
    h.store.enable_epic_account(session.clone());
    (h, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_confident_match_gets_an_epic_price_and_the_match_is_remembered() {
    let (h, _s) = with_account("good", "good").await;
    h.store.refresh_now(false).await.unwrap();

    // Survival Craft: equal title and the same maker -> priced from the search answer, in USD
    let g = h.store.game("steam:1").await.unwrap().game;
    let e = g.prices.iter().find(|p| p.store == StoreKind::Epic).expect("an Epic price");
    assert_eq!((e.native.minor, e.native.currency.as_str(), e.discount_pct), (1000, "USD", 50));
    assert_eq!(e.final_price.currency, "ZAR");
    assert_eq!(e.final_price.minor, 16000, "10 dollars at 16 rand");
    assert_eq!(e.original.minor, 32000);
    assert_eq!(g.cheapest, Some(StoreKind::Epic), "160 against 200 rand");
    assert!(e.url.ends_with("/p/slug-offer-survival"), "{}", e.url);

    // the others: wrong maker, ambiguous editions, only a different edition -> no Epic price at all
    for id in ["steam:2", "steam:3", "steam:5"] {
        let g = h.store.game(id).await.unwrap().game;
        assert!(g.prices.iter().all(|p| p.store == StoreKind::Steam), "{id}: {:?}", g.prices);
    }

    // Buy for the Steam game on Epic knows the Epic ids, so the store window can open the real checkout
    let t = h.store.buy_target("steam:1", StoreKind::Epic).unwrap();
    assert_eq!((t.namespace.as_deref(), t.offer_id.as_deref()), (Some("ns1"), Some("offer-survival")));

    // a second pass: matches and misses are remembered, so no search again; the match is priced by id
    let searches = h.srv.count("gql");
    assert!(searches >= 4, "each game was searched once: {searches}");
    h.srv.epic.lock().unwrap().offers[0].now = 800;
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.db().age_epic_quotes(7 * 3600).unwrap();
    h.store.refresh_now(false).await.unwrap();
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.srv.count("gql"), searches, "no more searches: the mapping is cached");
    assert!(h.srv.count("price-engine") >= 1, "priced by offer id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_token_is_refreshed_once_then_the_service_is_unavailable_never_guessed() {
    let (h, s) = with_account("stale", "good").await;
    h.store.refresh_now(false).await.unwrap();
    assert_eq!(s.refreshes.load(Ordering::SeqCst), 1, "refreshed once through the app's login, not by the store");
    let g = h.store.game("steam:1").await.unwrap().game;
    assert!(g.prices.iter().any(|p| p.store == StoreKind::Epic));
    // every call carried the bearer token in the header only; the stale one was tried first
    let log = h.srv.epic.lock().unwrap().log.clone();
    assert!(log.iter().any(|(_, t, _)| t == "stale") && log.iter().any(|(_, t, _)| t == "good"));
    assert!(log.iter().all(|(_, _, d)| !d.contains("good") && !d.contains("stale")), "a token never travels in a URL or body");

    // Epic refuses even the refreshed token: the price source gives up with an error, and nothing is invented
    let (h, s) = with_account("stale", "still-bad").await;
    h.store.refresh_now(false).await.unwrap();
    assert_eq!(s.refreshes.load(Ordering::SeqCst), 1, "one refresh, then the refusing routes are left alone: no refresh storm");
    let g = h.store.game("steam:1").await.unwrap().game;
    assert!(g.prices.iter().all(|p| p.store == StoreKind::Steam));
    let calls = h.srv.count("gql") + h.srv.count("price-engine") + h.srv.count("catalog") + h.srv.count("billing");
    h.store.db().age_epic_quotes(7 * 3600).unwrap();
    h.store.refresh_now(false).await.unwrap();
    assert_eq!(h.srv.count("gql") + h.srv.count("price-engine") + h.srv.count("catalog") + h.srv.count("billing"), calls, "not asked again for a while");
    assert_eq!(s.refreshes.load(Ordering::SeqCst), 1);
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_answers_when_the_price_engine_does_not() {
    let (h, _s) = with_account("good", "good").await;
    h.store.refresh_now(false).await.unwrap(); // the matches are made (search answers carry prices)
    {
        let mut e = h.srv.epic.lock().unwrap();
        e.price_engine_ok = false;
        e.offers[0].now = 600;
    }
    h.store.db().age_prices(7 * 3600).unwrap();
    h.store.db().age_epic_quotes(7 * 3600).unwrap();
    h.store.refresh_now(false).await.unwrap();
    assert!(h.srv.count("catalog") >= 1, "fell back to the catalog");
    let e = h.store.game("steam:1").await.unwrap().game.prices.into_iter().find(|p| p.store == StoreKind::Epic).unwrap();
    assert_eq!(e.native.minor, 600);
    assert_eq!(e.discount_pct, 70);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_is_priced_in_the_country_the_account_bills_in() {
    let (h, _s) = with_account("good", "good").await;
    h.srv.epic.lock().unwrap().billing_country = "MU".into();
    h.store.refresh_now(false).await.unwrap();
    assert_eq!(h.srv.count("billing"), 1);
    let log = h.srv.epic.lock().unwrap().log.clone();
    assert!(log.iter().any(|(r, _, d)| r == "gql" && d.ends_with("|MU")), "searches ask for the billing country: {log:?}");
    // asked once a day, not on every refresh
    h.store.refresh_now(true).await.unwrap();
    assert_eq!(h.srv.count("billing"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_offers_on_the_wishlist_are_priced_by_id_and_alert_on_a_sale() {
    let (h, _s) = with_account("good", "good").await;
    h.store.refresh_now(false).await.unwrap();
    let f = h.store.front().await.unwrap();
    let sker = f.epic_free.iter().find(|e| e.game.name == "Sker Ritual").expect("upcoming free game").game.clone();
    let (ns, offer) = {
        let mut p = sker.key.split(':').skip(1);
        (p.next().unwrap().to_string(), p.next().unwrap().to_string())
    };
    h.store.wishlist_set(&sker.key, true).await.unwrap();
    // Epic's price engine knows the offer and has it at half price
    h.srv.epic.lock().unwrap().offers.push(eoffer(&ns, &offer, "Sker Ritual", &["x"], 2000, 1000));
    h.store.wishlist_tick_now().await;
    assert!(h.srv.count("price-engine") >= 1);
    let sales = h.sales();
    let a = sales.iter().find(|a| a.key == sker.key).expect("a sale alert for the Epic offer");
    assert_eq!((a.store, a.discount_pct), (StoreKind::Epic, 50));
    let wl = h.store.wishlist().await.unwrap();
    let g = wl.iter().find(|g| g.key == sker.key).unwrap();
    assert_eq!(g.prices[0].native.minor, 1000);
    assert_eq!(g.prices[0].native.currency, "USD");
    h.store.wait_idle(IDLE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_an_epic_login_nothing_is_asked_of_epic() {
    let (h, s) = with_account("good", "good").await;
    s.signed_in.store(false, Ordering::SeqCst);
    assert_eq!(h.store.epic_source_name(), "none");
    h.store.refresh_now(false).await.unwrap();
    assert_eq!(h.srv.count("gql") + h.srv.count("price-engine") + h.srv.count("catalog") + h.srv.count("billing"), 0);
    let g = h.store.game("steam:1").await.unwrap().game;
    assert_eq!(g.prices.len(), 1);
    assert!(!grapple_store::epic::Unavailable.available());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_epic_login_is_tried_before_isthereanydeal_and_itad_fills_the_rest() {
    let (h, _s) = with_account("good", "good").await;
    *h.srv.itad_epic_price.lock().unwrap() = Some((1500, 1500, 0)); // $15 for everything ITAD knows
    h.store.itad_key_set(Some("itad-key".into())).await.unwrap();
    h.store.wait_idle(IDLE).await;
    assert_eq!(h.store.epic_source_name(), "epic-account");
    h.store.refresh_now(false).await.unwrap();
    // the matched game uses Epic's own price (10 dollars), the unmatched ones ITAD's
    let one = h.store.game("steam:1").await.unwrap().game;
    assert_eq!(one.prices.iter().find(|p| p.store == StoreKind::Epic).unwrap().native.minor, 1000);
    let two = h.store.game("steam:2").await.unwrap().game;
    assert_eq!(two.prices.iter().find(|p| p.store == StoreKind::Epic).unwrap().native.minor, 1500, "ITAD answers for the game Epic's search could not confirm");
}
