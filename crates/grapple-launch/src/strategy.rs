//! The boundary between "how a store starts a game" and "how Grapple tracks it".

use crate::error::Result;
use crate::proc::{Matcher, ProcFs};
use grapple_core::{Game, LaunchEvent};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a strategy gets to work with.
pub struct StartCtx {
    pub events: mpsc::Sender<LaunchEvent>,
    pub proc: ProcFs,
    /// Set to true when Grapple itself starts the Steam client (so "close Steam when I quit" knows it may).
    pub steam_started_by_us: Arc<AtomicBool>,
    /// Where per-game logs go.
    pub log_dir: PathBuf,
}

impl StartCtx {
    /// Tell the UI about a launch step.
    pub async fn step(&self, label: &str, done: bool) {
        let _ = self.events.send(LaunchEvent::Step { label: label.to_string(), done }).await;
    }
}

/// A started launch: how to find the game's processes and what to wait for.
pub struct Started {
    pub matcher: Matcher,
    /// Exit code watcher of the process Grapple spawned for the game (None when a store client starts it: Steam).
    pub child_exit: Option<watch::Receiver<Option<i32>>>,
    /// Give up if no game process shows up in this time.
    pub appear_timeout: Duration,
    /// If the spawned process dies before this much time, the launch counts as failed.
    pub min_alive: Duration,
    pub log: Option<PathBuf>,
    /// Label of the last step, marked done when the game is running.
    pub final_step: &'static str,
    /// The game starts through umu-run, which prints to `log` while it checks or downloads the Steam Linux
    /// Runtime: follow it (a download is a step, not a timeout, and a failed one is a clear error).
    pub umu_log: bool,
    /// Ask GameMode for the game's processes once they run (see [`crate::gamemode`]).
    pub gamemode: Option<crate::gamemode::Request>,
}

/// The label of the last step; the UI ticks it when the game is running.
pub const STEP_STARTING: &str = "Starting game";
/// umu is downloading (or unpacking) the Steam Linux Runtime before it can start the game.
pub const STEP_RUNTIME: &str = "Updating Steam Linux Runtime";

pub trait LaunchStrategy: Send + Sync {
    fn name(&self) -> &'static str;

    /// Do everything needed to get the game going (sign-in, runtime, spawning), emitting `Step` events.
    fn start<'a>(&'a self, game: &'a Game, ctx: &'a StartCtx) -> BoxFuture<'a, Result<Started>>;
}
