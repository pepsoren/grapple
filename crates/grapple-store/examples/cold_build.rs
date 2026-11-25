//! Measures what a cold store front costs against the real Steam/Epic/FX endpoints.
//!
//! Reads Josh's Steam/Epic library files (read-only, through grapple-scan) so owned games are excluded and the
//! taste profile is real, but keeps everything it caches in a temp directory: his Grapple is untouched. No login,
//! no cookies, nothing that changes an account.
//!
//!     GRAPPLE_STORE_CC=ZA nice -n 19 cargo run -p grapple-store --example cold_build --release
//!
//! Prints how many requests each service saw, how long the build took, and what the front holds.

use grapple_core::Store;
use grapple_store::{Friend, Library, OwnedGame, StoreConfig, StoreEvent, StoreService};
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Lib {
    owned: Vec<OwnedGame>,
    steam_root: Option<std::path::PathBuf>,
}

impl Library for Lib {
    fn owned(&self) -> Vec<OwnedGame> {
        self.owned.clone()
    }
    fn friends(&self) -> Vec<Friend> {
        Vec::new()
    }
    fn steam_id64(&self) -> Option<u64> {
        Some(grapple_store::steam::steam_id64(grapple_store::local::account_id(self.steam_root.as_deref()?)?))
    }
    fn steam_region(&self) -> Option<(String, &'static str)> {
        grapple_store::local::detect_country(self.steam_root.as_deref()?)
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let cfg = grapple_scan::ScanConfig::detect();
    let scan = grapple_scan::scan_all(&cfg);
    let owned: Vec<OwnedGame> = scan
        .games
        .iter()
        .filter(|g| matches!(g.store, Store::Steam | Store::Epic))
        .map(|g| OwnedGame {
            id: g.id.0.clone(),
            name: g.name.clone(),
            playtime_min: g.store_playtime_min,
            last_played: g.last_played,
            added_at: None,
            icon: None,
            epic_namespace: None,
        })
        .collect();
    println!("library: {} owned games (Steam {}, Epic {})", owned.len(), owned.iter().filter(|g| g.id.starts_with("steam:")).count(), owned.iter().filter(|g| g.id.starts_with("epic:")).count());

    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(0usize));
    let ev = events.clone();
    let lib = Arc::new(Lib { owned, steam_root: cfg.steam_root });
    let keys = Arc::new(grapple_store::secrets::MemoryKeys::new());
    let store = StoreService::new(
        StoreConfig::real(dir.path().join("data"), dir.path().join("art")),
        lib,
        keys,
        Arc::new(move |e| {
            if matches!(e, StoreEvent::Changed(_)) {
                *ev.lock().unwrap() += 1;
            }
        }),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let r = store.region();
    println!("region: {} / {} ({})", r.country, r.currency, r.source);

    let t = Instant::now();
    let cold = store.front().await.unwrap();
    println!("first front(): {:?}, {} picks, {} top (cold, answers at once)", t.elapsed(), cold.picked.len(), cold.top.len());
    // A timeline while it builds: when does the page become useful?
    loop {
        if store.wait_idle(std::time::Duration::from_secs(10)).await {
            break;
        }
        let f = store.front().await.unwrap();
        println!(
            "  +{:>3}s  {:>3} requests  picked {} top {} deals {} under {} new {} wishlist {} (named {})",
            t.elapsed().as_secs(),
            store.request_counts().steam,
            f.picked.len(),
            f.top.len(),
            f.deals.len(),
            f.under.games.len(),
            f.new_releases.games.len(),
            f.wishlist.len(),
            f.wishlist.iter().filter(|g| !g.name.is_empty()).count()
        );
    }
    println!("cold build finished in {:?}", t.elapsed());
    let t2 = Instant::now();
    let f = store.front().await.unwrap();
    println!("warm front(): {:?}", t2.elapsed());

    let c = store.request_counts();
    println!("requests: steam {} (of which store pages for tags: {}), cdn {}, other (epic/fx/itad) {}", c.steam, store.tag_pages(), c.cdn, c.other);
    println!("by kind: {:?}", store.request_kinds());
    println!("store-changed events: {}", events.lock().unwrap());
    let show = |name: &str, g: &[grapple_store::StoreGame]| {
        println!("{name} ({}):", g.len());
        for x in g.iter().take(12) {
            let p = x.prices.first().map(|p| format!("{} {:.2}{}", p.final_price.currency, p.final_price.minor as f64 / 100.0, if p.discount_pct > 0 { format!(" (-{}%)", p.discount_pct) } else { String::new() })).unwrap_or_else(|| "-".into());
            let why = x.similar.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
            println!("   {:<44} {:<22} {}", x.name, p, if why.is_empty() { String::new() } else { format!("like: {why}") });
        }
    };
    show("featured", &f.featured);
    show("picked", &f.picked);
    show("top", &f.top);
    show("deals", &f.deals);
    show(&format!("under (tier {}, {} hidden)", f.under.tier_minor, f.under.hidden), &f.under.games);
    show(&format!("new releases ({} hidden)", f.new_releases.hidden), &f.new_releases.games);
    show("wishlist", &f.wishlist);
    println!("epic free:");
    for e in &f.epic_free {
        println!("   {} now={} until={:?}", e.game.name, e.now, e.until);
    }
}
