//! Print the exact command line and environment a launch WOULD run. Starts nothing, calls no network API,
//! never reads Epic's `user.json` (account and token values are placeholders).
//!
//!   cargo run -p grapple-launch --example plan -- steam:730
//!   cargo run -p grapple-launch --example plan -- epic:Sugar
//!   cargo run -p grapple-launch --example plan -- manual:<id>   (needs an entry; only for tests)

use grapple_core::{Art, Game, GameId, Store};
use grapple_launch::epic::{EpicStrategy, PlanIdentity};
use grapple_launch::plan::LaunchPlan;
use grapple_launch::steam::SteamStrategy;
use grapple_launch::LaunchSettings;

fn game(id: &str) -> Game {
    let (store, store_id) = match id.split_once(':') {
        Some(("steam", s)) => (Store::Steam, s.to_string()),
        Some(("epic", s)) => (Store::Epic, s.to_string()),
        Some(("manual", s)) => (Store::Manual, s.to_string()),
        _ if id.chars().all(|c| c.is_ascii_digit()) => (Store::Steam, id.to_string()),
        _ => (Store::Epic, id.to_string()),
    };
    Game {
        id: GameId::new(store, &store_id),
        store,
        name: store_id.clone(),
        sort_name: store_id,
        installed: true,
        install_dir: None,
        size_bytes: None,
        art: Art::default(),
        store_playtime_min: 0,
        last_played: None,
        status: None,
    }
}

fn print(title: &str, plan: &LaunchPlan) {
    println!("== {title}");
    println!("command: {}", plan.display_command());
    if let Some(cwd) = &plan.cwd {
        println!("cwd:     {}", cwd.display());
    }
    println!("env (overrides on top of your session environment):");
    for (k, v) in &plan.env {
        println!("  {k}={v}");
    }
    for n in &plan.notes {
        println!("note: {n}");
    }
    println!();
}

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: plan <game-id>   e.g. steam:730 or epic:Sugar");
        std::process::exit(2);
    };
    let game = game(&arg);
    let settings = LaunchSettings::default();
    match game.store {
        Store::Steam => {
            let strategy = SteamStrategy::new(settings.steam.clone());
            let running = settings.steam.is_running(&settings.proc);
            println!("Steam is currently {}.\n", if running { "running" } else { "not running" });
            print("Steam already running (warm)", &strategy.plan(&game, true));
            print("Steam not running (cold start, hidden)", &strategy.plan(&game, false));
        }
        Store::Epic => {
            let strategy = EpicStrategy::new(settings);
            match strategy.plan(game.id.store_id(), &PlanIdentity::Placeholder, false, None) {
                Ok(plan) => {
                    print("Epic, online", &plan);
                    if let Ok(off) = strategy.plan(game.id.store_id(), &PlanIdentity::Placeholder, true, None) {
                        print("Epic, offline fallback (no token)", &off);
                    }
                    println!("Before this, a real launch signs in: refresh the access token if it expires within 10 minutes");
                    println!("(rotated tokens are written back to legendary's user.json), then GET /account/api/oauth/exchange");
                    println!("for the one-time code that replaces <exchange-code>.");
                }
                Err(e) => {
                    eprintln!("cannot plan: {e}");
                    std::process::exit(1);
                }
            }
        }
        Store::Manual => {
            eprintln!("manual games need an entry from the app's database; nothing to print here");
            std::process::exit(1);
        }
    }
}
