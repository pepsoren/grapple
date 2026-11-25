//! Steam: games need the Steam client (DRM, online), so Grapple runs it hidden and asks it to start the game.
//!
//! Findings of `docs/steam-research.md` applied here:
//! * no lighter client exists (SteamCMD `app_run` does not serve Steamworks games, `-no-browser` is dead),
//!   so there is only this one strategy;
//! * `-silent` (and `-nofriendsui`) are added only when Grapple itself has to start Steam;
//! * "Keep Steam ready" is off by default and costs about 1.3 GB of memory while idle;
//! * "Close Steam when I quit" defaults to on only for a Steam that Grapple started, and uses `steam -shutdown`.
//!
//! Every `steam` command is started outside Grapple ([`spawn_outside`]): clean session environment, own scope,
//! not a child of Grapple. A cold start is two steps: `steam -silent -nofriendsui` first, then, once the client
//! is up (its pid file names a live `steam` that has `~/.steam/steam.pipe` open, the same test Steam's own
//! `steam.sh` uses), `steam -applaunch <appid>`, which the client takes over its pipe.
//!
//! Every flag lives in [`SteamFlags`], so measuring `-applaunch` against `steam://rungameid/` is a one-line change.
//!
//! Windows: the same commands on `steam.exe` (from Steam's registry values). Steam has no pid file or pipe there; it
//! records itself under `HKCU\Software\Valve\Steam\ActiveProcess` instead (`pid` of the client, `ActiveUser` once
//! an account is signed in), so "ready" is a live `steam.exe` at that pid with a signed-in user. A running client
//! takes `steam.exe -applaunch <appid>` from a second `steam.exe` the same way.

use crate::env::child_path;
use crate::error::{LaunchError, Result};
use crate::paths;
use crate::proc::{ProcFs, steam_pid};
use crate::spawn::spawn_outside;
use crate::strategy::{BoxFuture, LaunchStrategy, STEP_STARTING, StartCtx, Started};
use grapple_core::Game;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The Steam step of the launch card. Not done while a cold Steam boots.
pub const STEP_STEAM_READY: &str = "Fetching game from Steam";
pub const STEP_FILES_READY: &str = "Game files ready";

/// Shown next to the "Keep Steam ready" setting.
pub const KEEP_STEAM_READY_DESCRIPTION: &str =
    "Start Steam hidden when Grapple starts, so games launch faster. Uses about 1.3 GB of memory while idle.";

/// How Steam is asked to start the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteamLaunchVia {
    /// `steam [-silent ...] -applaunch <appid>`
    Applaunch,
    /// `steam [-silent ...] steam://rungameid/<appid>`
    RunGameIdUrl,
}

/// All Steam command-line pieces in one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SteamFlags {
    pub launch_via: SteamLaunchVia,
    /// Added only when Grapple has to start Steam (it was not running).
    pub cold_start: Vec<String>,
    /// Used for "Keep Steam ready" (start Steam alone, hidden).
    pub keep_ready: Vec<String>,
    /// `-devtools-port <n>` for every Steam grapple starts: the Steam bridge's own loopback port, so it never
    /// collides with whatever else uses Steam's default 8080. `None`: Steam's default.
    pub devtools_port: Option<u16>,
    pub applaunch: String,
    pub url_prefix: String,
    /// Clean exit. Never kill Steam: that can lose the data offline mode needs.
    pub shutdown: String,
}

impl Default for SteamFlags {
    fn default() -> Self {
        SteamFlags {
            launch_via: SteamLaunchVia::Applaunch,
            cold_start: vec!["-silent".into(), "-nofriendsui".into()],
            keep_ready: vec!["-silent".into(), "-nofriendsui".into()],
            devtools_port: None,
            applaunch: "-applaunch".into(),
            url_prefix: "steam://rungameid/".into(),
            shutdown: "-shutdown".into(),
        }
    }
}

impl SteamFlags {
    /// Arguments for `steam` to start `appid`. `cold`: Grapple is starting Steam too.
    pub fn launch_args(&self, appid: &str, cold: bool) -> Vec<String> {
        let mut a: Vec<String> = if cold { self.with_port(self.cold_start.clone()) } else { Vec::new() };
        match self.launch_via {
            SteamLaunchVia::Applaunch => {
                a.push(self.applaunch.clone());
                a.push(appid.to_string());
            }
            SteamLaunchVia::RunGameIdUrl => a.push(format!("{}{appid}", self.url_prefix)),
        }
        a
    }
    pub fn ready_args(&self) -> Vec<String> {
        self.with_port(self.keep_ready.clone())
    }
    fn with_port(&self, mut a: Vec<String>) -> Vec<String> {
        if let Some(p) = self.devtools_port {
            a.push("-devtools-port".into());
            a.push(p.to_string());
        }
        a
    }
    pub fn shutdown_args(&self) -> Vec<String> {
        vec![self.shutdown.clone()]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SteamSettings {
    /// The `steam` command (looked up on `PATH` unless it contains a slash).
    pub bin: String,
    /// `~/.steam/steam.pid`
    pub pid_file: PathBuf,
    /// `~/.steam/steam.pipe`: the running client keeps it open and reads commands (`-applaunch`) from it.
    pub pipe_file: PathBuf,
    pub flags: SteamFlags,
    /// How long to wait for a cold Steam to come up.
    pub boot_timeout: Duration,
    /// How long to wait for the game's process after asking Steam to start it.
    pub game_timeout: Duration,
    /// After the last Steam game ends, wait this long before `steam -shutdown` (cloud sync, playtime).
    pub shutdown_delay: Duration,
}

/// The `steam` command: `steam` on `PATH`; on Windows Steam's own `steam.exe` from the registry.
fn default_bin() -> String {
    #[cfg(windows)]
    {
        use grapple_core::winreg::{Hive, read_string};
        if let Some(exe) = read_string(Hive::CurrentUser, crate::winproc::STEAM_KEY, "SteamExe").filter(|e| !e.trim().is_empty()) {
            return exe.replace('/', "\\");
        }
        if let Some(dir) = grapple_core::winreg::steam_dir() {
            return dir.join("steam.exe").to_string_lossy().into_owned();
        }
        "steam.exe".into()
    }
    #[cfg(not(windows))]
    "steam".into()
}

impl Default for SteamSettings {
    fn default() -> Self {
        SteamSettings {
            bin: default_bin(),
            pid_file: paths::home().join(".steam").join("steam.pid"),
            pipe_file: paths::home().join(".steam").join("steam.pipe"),
            flags: SteamFlags::default(),
            boot_timeout: Duration::from_secs(90),
            game_timeout: Duration::from_secs(180),
            shutdown_delay: Duration::from_secs(5),
        }
    }
}

impl SteamSettings {
    pub fn is_running(&self, fs: &ProcFs) -> bool {
        steam_pid(fs, &self.pid_file).is_some()
    }

    /// Running and ready for commands: the client named by the pid file has its command pipe open.
    #[cfg(not(windows))]
    pub fn is_ready(&self, fs: &ProcFs) -> bool {
        let Some(pid) = steam_pid(fs, &self.pid_file) else { return false };
        let Ok(rd) = std::fs::read_dir(fs.root().join(pid.to_string()).join("fd")) else { return false };
        rd.filter_map(|e| e.ok()).any(|e| std::fs::read_link(e.path()).is_ok_and(|t| t == self.pipe_file))
    }

    /// Running and ready for commands: `ActiveProcess` names a live `steam.exe` with a signed-in user.
    #[cfg(windows)]
    pub fn is_ready(&self, fs: &ProcFs) -> bool {
        let user = grapple_core::winreg::read_dword(grapple_core::winreg::Hive::CurrentUser, crate::winproc::STEAM_ACTIVE_KEY, "ActiveUser");
        crate::winproc::steam_ready(steam_pid(fs, &self.pid_file).is_some(), user)
    }

    /// Wait until [`Self::is_ready`], up to `boot_timeout`.
    pub async fn wait_ready(&self, fs: &ProcFs) -> Result<()> {
        let deadline = Instant::now() + self.boot_timeout;
        while !self.is_ready(fs) {
            if Instant::now() > deadline {
                return Err(LaunchError::Timeout("Steam did not start in time".into()));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(())
    }

    fn run(&self, path_var: &str, args: &[String]) -> Result<()> {
        let bin = self.resolve_bin(path_var)?;
        spawn_outside(&bin.to_string_lossy(), args)
    }

    /// Hand a `steam://` URL to Steam (e.g. `steam://install/<appid>`), started like every other Steam command.
    pub fn open_url(&self, url: &str) -> Result<()> {
        self.run(&child_path(), &[url.to_string()])
    }

    /// Resolve the `steam` binary (so errors are clear and tests can use a fake script).
    pub fn resolve_bin(&self, path_var: &str) -> Result<PathBuf> {
        paths::which_in(&self.bin, path_var).ok_or_else(|| LaunchError::Missing {
            what: "the steam command",
            detail: format!("`{}` is not on PATH; is Steam installed?", self.bin),
        })
    }

    /// Start Steam alone, hidden ("Keep Steam ready"). No-op when it already runs.
    pub async fn start_ready(&self, fs: &ProcFs, started_by_us: &std::sync::atomic::AtomicBool) -> Result<bool> {
        if self.is_running(fs) {
            return Ok(false);
        }
        self.run(&child_path(), &self.flags.ready_args())?;
        started_by_us.store(true, Ordering::SeqCst);
        Ok(true)
    }

    /// `steam -shutdown`: a clean exit through Steam itself.
    pub fn shutdown(&self, fs: &ProcFs) -> Result<()> {
        let _ = fs;
        self.run(&child_path(), &self.flags.shutdown_args())
    }
}

pub struct SteamStrategy {
    pub settings: SteamSettings,
    /// `PATH` used to find `steam` (the real one by default; tests pass a directory with a fake script).
    pub path_var: String,
}

impl SteamStrategy {
    pub fn new(settings: SteamSettings) -> Self {
        SteamStrategy { settings, path_var: child_path() }
    }

    /// The command that would run, without running it (used by the `plan` example and tests). For a cold Steam
    /// this is the second command; the note names the first.
    pub fn plan(&self, game: &Game, steam_running: bool) -> crate::plan::LaunchPlan {
        let appid = game.id.store_id();
        let program = self.settings.bin.clone();
        let mut notes = vec![if steam_running {
            "Steam is already running: the request is handed to it".to_string()
        } else {
            format!(
                "Steam is not running: Grapple first runs `{program} {}` (hidden) and waits until it has {} open",
                self.settings.flags.ready_args().join(" "),
                self.settings.pipe_file.display()
            )
        }];
        notes.push("every steam command runs outside Grapple: session environment, own scope, not Grapple's child".into());
        notes.push("the game is found afterwards by SteamAppId/SteamGameId or the `reaper SteamLaunch AppId=` parent".into());
        crate::plan::LaunchPlan {
            program,
            args: self.settings.flags.launch_args(appid, false).into_iter().map(crate::plan::PlanArg::Lit).collect(),
            env: BTreeMap::new(),
            cwd: None,
            notes,
            gamemode: None,
        }
    }
}

impl LaunchStrategy for SteamStrategy {
    fn name(&self) -> &'static str {
        "steam"
    }

    fn start<'a>(&'a self, game: &'a Game, ctx: &'a StartCtx) -> BoxFuture<'a, Result<Started>> {
        Box::pin(async move {
            let s = &self.settings;
            let appid = game.id.store_id().to_string();
            let ready = s.is_ready(&ctx.proc);

            ctx.step(STEP_STEAM_READY, ready).await;
            if !ready {
                // Not running: start it hidden on its own first. Running but still booting (Keep Steam ready, or
                // started by hand a moment ago): just wait for it.
                if !s.is_running(&ctx.proc) {
                    s.run(&self.path_var, &s.flags.ready_args())?;
                    ctx.steam_started_by_us.store(true, Ordering::SeqCst);
                }
                s.wait_ready(&ctx.proc).await?;
                ctx.step(STEP_STEAM_READY, true).await;
            }
            // The client takes this over its pipe; the `steam` process we start for it exits at once.
            s.run(&self.path_var, &s.flags.launch_args(&appid, false))?;

            // Steam checks the files itself before it starts the game (it shows "update queued" otherwise);
            // here we only confirm there is something to start.
            let present = game.installed && game.install_dir.as_ref().is_none_or(|d| d.exists());
            ctx.step(STEP_FILES_READY, present).await;
            ctx.step(STEP_STARTING, false).await;

            Ok(Started {
                matcher: crate::proc::Matcher::new(game.id.clone()),
                child_exit: None,
                appear_timeout: s.game_timeout,
                min_alive: Duration::ZERO,
                log: None,
                final_step: STEP_STARTING,
                umu_log: false,
                gamemode: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_and_cold_commands() {
        let f = SteamFlags::default();
        assert_eq!(f.launch_args("730", false), ["-applaunch", "730"]);
        assert_eq!(f.launch_args("730", true), ["-silent", "-nofriendsui", "-applaunch", "730"]);
        let f = SteamFlags { launch_via: SteamLaunchVia::RunGameIdUrl, ..f };
        assert_eq!(f.launch_args("730", true), ["-silent", "-nofriendsui", "steam://rungameid/730"]);
        assert_eq!(f.launch_args("730", false), ["steam://rungameid/730"]);
        let f = SteamFlags { devtools_port: Some(41234), ..SteamFlags::default() };
        assert_eq!(f.launch_args("730", true), ["-silent", "-nofriendsui", "-devtools-port", "41234", "-applaunch", "730"]);
        assert_eq!(f.launch_args("730", false), ["-applaunch", "730"], "a running Steam keeps its port");
        assert_eq!(f.ready_args(), ["-silent", "-nofriendsui", "-devtools-port", "41234"]);
    }
}
