//! `cargo run --release -p grapple-scan --example scan`: scan this machine and print counts, timing and the first games.
use grapple_scan::{ScanConfig, scan_all};
use std::time::Instant;

fn main() {
    let cfg = ScanConfig::detect();
    println!("steam_root     {:?}\nheroic_config  {:?}\nlegendary_dir  {:?}", cfg.steam_root, cfg.heroic_config, cfg.legendary_dir);

    let t = Instant::now();
    let cold = scan_all(&cfg);
    println!("\nfirst scan:  {:?}", t.elapsed());
    let mut best = std::time::Duration::MAX;
    let mut total = std::time::Duration::ZERO;
    const RUNS: u32 = 20;
    for _ in 0..RUNS {
        let t = Instant::now();
        let r = scan_all(&cfg);
        let d = t.elapsed();
        std::hint::black_box(&r);
        best = best.min(d);
        total += d;
    }
    println!("warm scan:   best {best:?}, mean {:?} over {RUNS} runs", total / RUNS);

    let res = cold;
    let steam: Vec<_> = res.games.iter().filter(|g| g.store == grapple_core::Store::Steam).collect();
    let epic: Vec<_> = res.games.iter().filter(|g| g.store == grapple_core::Store::Epic).collect();
    let installed = res.games.iter().filter(|g| g.installed).count();
    println!(
        "\n{} games: {} steam ({} installed), {} epic ({} installed); {} installed in total",
        res.games.len(),
        steam.len(),
        steam.iter().filter(|g| g.installed).count(),
        epic.len(),
        epic.iter().filter(|g| g.installed).count(),
        installed
    );
    let count = |f: fn(&grapple_core::Art) -> bool| res.games.iter().filter(|g| f(&g.art)).count();
    println!(
        "art: cover {}, hero {}, logo {}, icon {}",
        count(|a| a.cover.is_some()),
        count(|a| a.hero.is_some()),
        count(|a| a.logo.is_some()),
        count(|a| a.icon.is_some())
    );
    for w in &res.warnings {
        println!("warning: {w}");
    }

    let mut sorted: Vec<_> = res.games.iter().collect();
    sorted.sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| a.sort_name.cmp(&b.sort_name)));
    println!("\nfirst 10 (installed first):");
    for g in sorted.iter().take(10) {
        println!(
            "  {:<14} {:<34} installed={:<5} played={:>5}m last={:?} size={:?} status={:?}\n      cover={} hero={} logo={} icon={}",
            g.id.0,
            g.name,
            g.installed,
            g.store_playtime_min,
            g.last_played,
            g.size_bytes,
            g.status,
            g.art.cover.is_some(),
            g.art.hero.is_some(),
            g.art.logo.is_some(),
            g.art.icon.is_some()
        );
    }
}
