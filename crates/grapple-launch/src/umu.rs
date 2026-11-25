// SPDX-License-Identifier: GPL-3.0-or-later
// Behaviour reimplemented from umu-launcher (GPLv3); see NOTICE.md.
//! umu-launcher's Steam Linux Runtime: the container Proton runs in. Read from umu-launcher 1.4.0
//! (`umu/umu_run.py`, `umu/umu_runtime.py`, `umu/umu_consts.py`, `umu/umu_util.py`).
//!
//! * Every `umu-run` asks repo.steampowered.com whether its runtime is current *before* it starts the game
//!   (`umu_run` waits on `setup_umu` -> `_update_umu` -> `_update_umu_platform`): it fetches
//!   `latest-public-beta/VERSION.txt` and, when that string is not in the local `VERSIONS.txt`, downloads the
//!   whole runtime (~175 MB) while the game waits. A launch with a complete runtime therefore sets
//!   `UMU_RUNTIME_UPDATE=0` ([`runtime_check`]); `setup_umu` then returns at once.
//! * The runtime is kept current in the background instead ([`Updater`], driven by the launcher while no game
//!   runs), with umu's own updater.
//! * What umu prints while it downloads ([`classify`], [`RuntimeWatch`]) becomes a launch step, so a download is
//!   shown, does not count against the launch timeout, and a failed one is reported as such.

use crate::paths;
use crate::plan::{LaunchPlan, PlanArg};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A Steam Linux Runtime umu knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runtime {
    /// Its folder in umu's dir, and umu's `RUNTIMEPATH` name for it (`steamrt4`).
    pub variant: &'static str,
    /// Prefix of its platform folder (`steamrt4_platform_4.0.…`, `sniper_platform_0.…`).
    pub codename: &'static str,
}

/// umu_runtime.py `RUNTIME_VERSIONS`: the Steam tool appid a Proton's `toolmanifest.vdf` requires.
const RUNTIMES: &[(&str, Runtime)] = &[
    ("1391110", Runtime { variant: "steamrt2", codename: "soldier" }),
    ("1628350", Runtime { variant: "steamrt3", codename: "sniper" }),
    ("3810310", Runtime { variant: "steamrt3-arm64", codename: "sniper" }),
    ("4183110", Runtime { variant: "steamrt4", codename: "steamrt4" }),
    ("4185400", Runtime { variant: "steamrt4-arm64", codename: "steamrt4" }),
];

/// umu's own folder (umu_consts.py `UMU_LOCAL`): `$UMU_FOLDERS_PATH/umu`, else `$XDG_DATA_HOME/umu`.
pub fn default_dir() -> PathBuf {
    match crate::env::child_var("UMU_FOLDERS_PATH") {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("umu"),
        _ => paths::data_home().join("umu"),
    }
}

/// The runtime a Proton runs in (`require_tool_appid` in its `toolmanifest.vdf`). None: no container (old
/// Protons, plain Wine) or one umu does not know.
pub fn required_runtime(proton_dir: &Path) -> Option<Runtime> {
    let text = std::fs::read_to_string(proton_dir.join("toolmanifest.vdf")).ok()?;
    let appid = text.lines().find_map(|l| {
        // `  "require_tool_appid" "4183110"`
        let mut quoted = l.split('"').skip(1).step_by(2);
        (quoted.next()? == "require_tool_appid").then(|| quoted.next().map(str::to_string)).flatten()
    })?;
    RUNTIMES.iter().find(|(id, _)| *id == appid).map(|(_, rt)| *rt)
}

fn platform_dir(dir: &Path, codename: &str) -> Option<PathBuf> {
    let prefix = format!("{codename}_platform_");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix) && e.path().is_dir())
        .map(|e| e.path())
        .max()
}

/// Is `rt` installed completely, so umu can start a game from it without its update check? Everything umu
/// itself checks before it would download or repair: the install marker (written only after `pv-verify`
/// passed: umu_util.py `has_runtime_installed`), `mtree.txt.gz` (`UmuRuntime.__bool__`), `pressure-vessel/`,
/// `VERSIONS.txt` and a `<codename>_platform_*` folder (`_update_umu`), and the entry point it runs.
pub fn is_ready(umu_dir: &Path, rt: Runtime) -> bool {
    let d = umu_dir.join(rt.variant);
    let file = |n: &str| d.join(n).is_file();
    file(".installed.ok")
        && file("mtree.txt.gz")
        && file("VERSIONS.txt")
        && file("toolmanifest.vdf")
        && file("_v2-entry-point")
        && d.join("pressure-vessel").is_dir()
        && platform_dir(&d, rt.codename).is_some_and(|p| p.join("files").is_dir())
}

/// What a launch through umu does about the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCheck {
    /// Installed and complete: start without asking the server (`UMU_RUNTIME_UPDATE=0`).
    Skip(Runtime),
    /// Missing or incomplete: umu downloads or repairs it first, and the launch shows that.
    Needed(Runtime),
    /// The Proton needs no container umu manages.
    NoRuntime,
}

pub fn runtime_check(proton_dir: &Path, umu_dir: &Path) -> RuntimeCheck {
    match required_runtime(proton_dir) {
        Some(rt) if is_ready(umu_dir, rt) => RuntimeCheck::Skip(rt),
        Some(rt) => RuntimeCheck::Needed(rt),
        None => RuntimeCheck::NoRuntime,
    }
}

/// Put the decision into a launch's environment. A `UMU_RUNTIME_UPDATE` the user set (Heroic's environment
/// options) is kept.
pub fn apply(check: RuntimeCheck, env: &mut BTreeMap<String, String>, notes: &mut Vec<String>) {
    match check {
        RuntimeCheck::Skip(rt) => {
            if !env.contains_key("UMU_RUNTIME_UPDATE") {
                env.insert("UMU_RUNTIME_UPDATE".into(), "0".into());
                notes.push(format!("{}: installed; no update check before the game (updated in the background)", rt.variant));
            }
        }
        RuntimeCheck::Needed(rt) => notes.push(format!("{}: not installed yet; umu downloads it before the game starts", rt.variant)),
        RuntimeCheck::NoRuntime => {}
    }
}

// ---------------------------------------------------------------- what umu prints

/// A line of umu's output that matters for a launch (umu_runtime.py / umu_run.py messages, 1.4.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UmuLine {
    /// umu is fetching the runtime and the game waits.
    Downloading,
    /// Downloaded; umu unpacks, swaps in and verifies it (seconds; must not be interrupted).
    Installing,
    /// Setup is over: umu starts the program now.
    Ready,
    /// The download failed. With an older runtime umu carries on with it; without one it gives up.
    Failed,
}

pub fn classify(line: &str) -> Option<UmuLine> {
    let l = line.trim();
    let has = |s: &str| l.contains(s);
    if has("Aborting steamrt install")
        || has("Digest mismatched")
        || has("umu has not been setup for the user")
        || has("An internet connection is required")
    {
        Some(UmuLine::Failed)
    } else if has("SHA256 is OK") || has("Verifying integrity of") {
        Some(UmuLine::Installing)
    } else if (has("Updating ") && has(" to latest"))
        || (has("Downloading ") && has("please wait"))
        || has("Setting up Unified Launcher")
        || has("Restoring Runtime Platform")
        || has("Connection broken, trying to resume")
        || (has("Found '") && has("in cache, resuming"))
    {
        Some(UmuLine::Downloading)
    } else if has(" is up to date")
        || has("updates disabled, skipping")
        || has(" was restored")
        || (has("Using ") && has("(latest)"))
        || (has("Running '") && has("using runtime"))
    {
        Some(UmuLine::Ready)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Nothing about the runtime yet.
    #[default]
    Quiet,
    Downloading,
    Installing,
    /// umu is past its setup.
    Ready,
}

/// Follows umu's output during one launch and turns it into the runtime step.
#[derive(Debug, Clone, Default)]
pub struct RuntimeWatch {
    pub phase: Phase,
    /// The step was shown.
    pub shown: bool,
    /// A download failed (umu may still go on with an older runtime).
    pub failed: bool,
}

impl RuntimeWatch {
    /// Take one line; returns the step update to send (`Some(done)`).
    pub fn feed(&mut self, line: &str) -> Option<bool> {
        match classify(line)? {
            UmuLine::Downloading | UmuLine::Installing if self.phase == Phase::Ready => None,
            UmuLine::Downloading => {
                self.phase = Phase::Downloading;
                self.failed = false;
                self.show()
            }
            UmuLine::Installing => {
                self.phase = Phase::Installing;
                self.show()
            }
            UmuLine::Ready => {
                self.phase = Phase::Ready;
                self.shown.then_some(true)
            }
            UmuLine::Failed => {
                self.failed = true;
                if self.phase != Phase::Ready {
                    self.phase = Phase::Quiet;
                }
                None
            }
        }
    }

    fn show(&mut self) -> Option<bool> {
        (!std::mem::replace(&mut self.shown, true)).then_some(false)
    }

    /// umu is getting the runtime: the game cannot be up yet, and the wait is not the game's.
    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Downloading | Phase::Installing)
    }

    pub fn ready(&self) -> bool {
        self.phase == Phase::Ready
    }
}

/// Reads what was appended to a log file since the last call, line by line. Cheap: one `stat`-sized read per
/// call, nothing kept but the position and an unfinished line.
pub struct LogTail {
    path: PathBuf,
    pos: u64,
    partial: String,
}

/// At most this much is read per call (umu writes a few lines; a chatty game cannot make a poll slow).
const TAIL_CHUNK: u64 = 64 * 1024;

impl LogTail {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        LogTail { path: path.into(), pos: 0, partial: String::new() }
    }

    pub fn lines(&mut self) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return Vec::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.pos {
            // truncated (a new launch wrote a fresh log)
            self.pos = 0;
            self.partial.clear();
        }
        if len == self.pos || f.seek(SeekFrom::Start(self.pos)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        let n = f.take(TAIL_CHUNK).read_to_end(&mut buf).unwrap_or(0);
        self.pos += n as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut out: Vec<String> = Vec::new();
        while let Some(i) = self.partial.find(['\n', '\r']) {
            let line: String = self.partial.drain(..=i).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if !line.is_empty() {
                out.push(line.to_string());
            }
        }
        out
    }
}

/// The phase umu reached in a whole log (the background updater's).
pub fn phase_of(log: &Path) -> RuntimeWatch {
    let mut w = RuntimeWatch::default();
    for l in LogTail::new(log).lines() {
        w.feed(&l);
    }
    w
}

// ---------------------------------------------------------------- background updates

/// How often the runtime is checked at most, and how soon a failed check is tried again.
pub const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);
pub const RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Keeps umu's runtimes current while no game runs, with umu's own updater: `umu-run true` with
/// `UMU_NO_PROTON=1` and `RUNTIMEPATH=<variant>` makes umu set up its pass-through tool for that runtime
/// (umu_run.py `resolve_runtime`, umu_proton.py `_get_umu_runtime_tool`), run `setup_umu` with the usual update
/// check and download, then run `true` in the container and exit. No Proton, no prefix, no window.
///
/// Interrupting it is safe: umu downloads to a temporary file, unpacks into a temporary folder next to the
/// runtime and swaps the two with `renameat2(RENAME_EXCHANGE)`; the install marker follows `pv-verify`, so
/// a runtime caught between swap and marker is merely re-verified (and never skipped, see [`is_ready`]) on
/// the next launch. The launcher still only stops it while it downloads, never while it unpacks.
#[derive(Debug, Clone)]
pub struct Updater {
    pub umu_run: PathBuf,
    pub umu_dir: PathBuf,
    /// Output of each run: `<log_dir>/umu-<variant>.log`.
    pub log_dir: PathBuf,
}

impl Updater {
    /// Runtimes umu has set up here before (their folders exist). Only the ones `RUNTIMEPATH` names
    /// (umu_run.py `resolve_runtime`: steamrt2, steamrt3, steamrt4), matching this machine.
    pub fn runtimes(&self) -> Vec<Runtime> {
        RUNTIMES
            .iter()
            .map(|(_, rt)| *rt)
            .filter(|rt| matches!(rt.variant, "steamrt2" | "steamrt3" | "steamrt4") && cfg!(target_arch = "x86_64"))
            .filter(|rt| self.umu_dir.join(rt.variant).is_dir())
            .collect()
    }

    pub fn log(&self, rt: Runtime) -> PathBuf {
        self.log_dir.join(format!("umu-{}.log", rt.variant))
    }

    /// Written after a run that ended with the runtime current; its age says when the next check is due.
    pub fn stamp(&self, rt: Runtime) -> PathBuf {
        self.log_dir.join(format!("umu-{}.checked", rt.variant))
    }

    /// Is a check of `rt` due? `last_try`: how long ago the last attempt (successful or not) started.
    pub fn due(&self, rt: Runtime, now: SystemTime, last_try: Option<Duration>) -> bool {
        let checked = std::fs::metadata(self.stamp(rt)).and_then(|m| m.modified()).ok();
        due(now, checked, last_try)
    }

    /// The command (see the type's docs). `UMU_ZENITY` is cleared: umu would show a download window.
    pub fn plan(&self, rt: Runtime) -> LaunchPlan {
        let mut env = BTreeMap::new();
        env.insert("UMU_NO_PROTON".to_string(), "1".to_string());
        env.insert("RUNTIMEPATH".to_string(), rt.variant.to_string());
        env.insert("UMU_ZENITY".to_string(), String::new());
        env.insert("GAMEID".to_string(), "umu-0".to_string());
        LaunchPlan {
            program: self.umu_run.to_string_lossy().into_owned(),
            args: vec![PlanArg::lit("true")],
            env,
            cwd: Some(self.umu_dir.clone()),
            notes: vec![format!("keeps {} current; runs nothing but `true` in it", rt.variant)],
            gamemode: None,
        }
    }

    /// Did a run end with the runtime current? (`exit`: umu-run's exit code.)
    pub fn succeeded(&self, rt: Runtime, exit: i32) -> bool {
        let w = phase_of(&self.log(rt));
        exit == 0 && w.ready() && !w.failed
    }

    pub fn mark_checked(&self, rt: Runtime) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.log_dir)?;
        std::fs::write(self.stamp(rt), b"")
    }
}

pub fn due(now: SystemTime, checked: Option<SystemTime>, last_try: Option<Duration>) -> bool {
    if last_try.is_some_and(|ago| ago < RETRY_AFTER) {
        return false;
    }
    checked.is_none_or(|t| now.duration_since(t).unwrap_or(Duration::ZERO) >= UPDATE_EVERY)
}

/// The user turned umu's updates off for everything (`UMU_RUNTIME_UPDATE=0` in the session environment).
pub fn updates_disabled_by_user() -> bool {
    crate::env::child_var("UMU_RUNTIME_UPDATE").is_some_and(|v| v == "0")
}

/// Processes that mean a game (or another launcher's container) is running: no runtime swaps under them.
pub fn is_container_process(comm: &str) -> bool {
    comm.starts_with("pv-adverb") || comm.starts_with("pressure-vessel") || comm.starts_with("pv-bwrap") || comm.starts_with("wineserver")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RT4: Runtime = Runtime { variant: "steamrt4", codename: "steamrt4" };

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    /// A runtime folder laid out like umu leaves it after a good install.
    fn installed_runtime(umu: &Path) {
        let d = umu.join("steamrt4");
        write(&d.join(".installed.ok"), "ok\n");
        write(&d.join("mtree.txt.gz"), "x");
        write(&d.join("VERSIONS.txt"), "depot\t4.0.20261110.268882\t\t\t# Overall version number\n");
        write(&d.join("toolmanifest.vdf"), "\"manifest\"\n{\n  \"commandline\" \"/_v2-entry-point --verb=%verb% --\"\n}\n");
        write(&d.join("_v2-entry-point"), "#!/bin/sh\n");
        std::fs::create_dir_all(d.join("pressure-vessel/bin")).unwrap();
        std::fs::create_dir_all(d.join("steamrt4_platform_4.0.20261110.268882/files")).unwrap();
    }

    fn ge_proton(dir: &Path) -> PathBuf {
        let p = dir.join("GE-Proton");
        write(
            &p.join("toolmanifest.vdf"),
            "\"manifest\"\n{\n  \"version\" \"2\"\n  \"commandline\" \"/proton %verb%\"\n  \"require_tool_appid\" \"4183110\"\n}\n",
        );
        p
    }

    #[test]
    fn the_runtime_comes_from_the_protons_manifest() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(required_runtime(&ge_proton(dir.path())), Some(RT4));
        write(&dir.path().join("old/toolmanifest.vdf"), "\"manifest\"\n{\n  \"commandline\" \"/proton %verb%\"\n}\n");
        assert_eq!(required_runtime(&dir.path().join("old")), None, "no container");
        assert_eq!(required_runtime(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_complete_runtime_skips_umus_update_check() {
        let dir = tempfile::tempdir().unwrap();
        let umu = dir.path().join("umu");
        let proton = ge_proton(dir.path());
        installed_runtime(&umu);
        assert_eq!(runtime_check(&proton, &umu), RuntimeCheck::Skip(RT4));
        let (mut env, mut notes) = (BTreeMap::new(), Vec::new());
        apply(runtime_check(&proton, &umu), &mut env, &mut notes);
        assert_eq!(env.get("UMU_RUNTIME_UPDATE").map(String::as_str), Some("0"));

        // the user's own choice stays
        let mut env = BTreeMap::from([("UMU_RUNTIME_UPDATE".to_string(), "1".to_string())]);
        apply(RuntimeCheck::Skip(RT4), &mut env, &mut notes);
        assert_eq!(env["UMU_RUNTIME_UPDATE"], "1");
    }

    #[test]
    fn a_missing_or_half_runtime_lets_umu_download_it() {
        let dir = tempfile::tempdir().unwrap();
        let umu = dir.path().join("umu");
        let proton = ge_proton(dir.path());
        assert_eq!(runtime_check(&proton, &umu), RuntimeCheck::Needed(RT4), "nothing installed");
        for missing in [".installed.ok", "mtree.txt.gz", "VERSIONS.txt", "pressure-vessel", "steamrt4_platform_4.0.20261110.268882"] {
            installed_runtime(&umu);
            let p = umu.join("steamrt4").join(missing);
            if p.is_dir() {
                std::fs::remove_dir_all(&p).unwrap();
            } else {
                std::fs::remove_file(&p).unwrap();
            }
            assert_eq!(runtime_check(&proton, &umu), RuntimeCheck::Needed(RT4), "without {missing}");
            let (mut env, mut notes) = (BTreeMap::new(), Vec::new());
            apply(runtime_check(&proton, &umu), &mut env, &mut notes);
            assert!(!env.contains_key("UMU_RUNTIME_UPDATE"), "without {missing}");
            std::fs::remove_dir_all(&umu).unwrap();
        }
        // a Proton without a container: nothing to decide
        assert_eq!(runtime_check(&dir.path().join("nothing"), &umu), RuntimeCheck::NoRuntime);
    }

    #[test]
    fn umus_messages_map_to_the_runtime_step() {
        // as umu 1.4.0 prints them (the user's Rocket League log, and umu_runtime.py)
        let lines = [
            ("ERROR: ld.so: object 'libgamemodeauto.so.0' from LD_PRELOAD cannot be preloaded (cannot open shared object file): ignored.", None),
            ("INFO: umu-launcher version 1.4.0 (3.13.15 (main, Aug  5 2026, 12:25:43) [GCC 15.3.0])", None),
            ("INFO: Updating steamrt4 to latest...", Some(UmuLine::Downloading)),
            ("INFO: Downloading steamrt4 (latest), please wait...", Some(UmuLine::Downloading)),
            ("INFO: Connection broken, trying to resume /tmp/tmpgijngcu2/SteamLinuxRuntime_4.tar.xz.4.0.20261110.268882.parts (retry 1)", Some(UmuLine::Downloading)),
            ("INFO: Setting up Unified Launcher for Windows Games on Linux...", Some(UmuLine::Downloading)),
            ("INFO: SteamLinuxRuntime_4.tar.xz: SHA256 is OK", Some(UmuLine::Installing)),
            ("INFO: Verifying integrity of steamrt4_platform_4.0.20261110.268882...", Some(UmuLine::Installing)),
            ("INFO: steamrt4 is up to date", Some(UmuLine::Ready)),
            ("INFO: Using steamrt4 (latest)", Some(UmuLine::Ready)),
            ("INFO: steamrt4 updates disabled, skipping", Some(UmuLine::Ready)),
            ("INFO: Running 'GE-Proton10-25' using runtime 'steamrt4'", Some(UmuLine::Ready)),
            ("ERROR: Aborting steamrt install due to network error", Some(UmuLine::Failed)),
            ("RuntimeError: umu has not been setup for the user", Some(UmuLine::Failed)),
            ("gamemodeauto: ", None),
        ];
        for (line, want) in lines {
            assert_eq!(classify(line), want, "{line}");
        }
    }

    #[test]
    fn the_watch_shows_the_step_once_and_ticks_it() {
        let mut w = RuntimeWatch::default();
        assert_eq!(w.feed("INFO: umu-launcher version 1.4.0"), None);
        assert!(!w.busy() && !w.ready());
        assert_eq!(w.feed("INFO: Updating steamrt4 to latest..."), Some(false));
        assert_eq!(w.feed("INFO: Downloading steamrt4 (latest), please wait..."), None, "shown once");
        assert!(w.busy());
        assert_eq!(w.feed("INFO: SteamLinuxRuntime_4.tar.xz: SHA256 is OK"), None);
        assert!(w.busy());
        assert_eq!(w.feed("INFO: steamrt4 is up to date"), Some(true));
        assert!(w.ready() && !w.busy());

        // a broken download: not busy any more, and remembered
        let mut w = RuntimeWatch::default();
        w.feed("INFO: Downloading steamrt4 (latest), please wait...");
        assert_eq!(w.feed("ERROR: Aborting steamrt install due to network error"), None);
        assert!(w.failed && !w.busy());
        // ... after which umu may go on with the runtime it had
        assert_eq!(w.feed("INFO: Running 'GE-Proton' using runtime 'steamrt4'"), Some(true));

        // the usual launch: nothing to show
        let mut w = RuntimeWatch::default();
        assert_eq!(w.feed("INFO: steamrt4 updates disabled, skipping"), None);
        assert!(w.ready() && !w.shown);
    }

    #[test]
    fn the_tail_returns_whole_new_lines_only() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("g.log");
        let mut t = LogTail::new(&log);
        assert!(t.lines().is_empty(), "no file yet");
        std::fs::write(&log, "a\nb").unwrap();
        assert_eq!(t.lines(), ["a"]);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"c\r\nd\n").unwrap();
        assert_eq!(t.lines(), ["bc", "d"]);
        assert!(t.lines().is_empty());
        std::fs::write(&log, "new\n").unwrap();
        assert_eq!(t.lines(), ["new"], "a truncated log starts over");
    }

    #[test]
    fn background_updates_run_daily_and_retry_hourly() {
        let now = SystemTime::now();
        let h = |n: u64| Duration::from_secs(n * 3600);
        assert!(due(now, None, None), "never checked");
        assert!(!due(now, Some(now - h(3)), None), "checked today");
        assert!(due(now, Some(now - h(25)), None));
        assert!(!due(now, Some(now - h(25)), Some(h(0))), "a try just now (it failed): wait");
        assert!(due(now, None, Some(h(2))), "failed two hours ago: try again");
    }

    #[test]
    fn the_updater_runs_umus_own_update_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let umu = dir.path().join("umu");
        let up = Updater { umu_run: dir.path().join("umu-run"), umu_dir: umu.clone(), log_dir: dir.path().join("logs") };
        assert!(up.runtimes().is_empty(), "umu never set anything up here");
        installed_runtime(&umu);
        std::fs::create_dir_all(umu.join("steamrt4-arm64")).unwrap();
        if cfg!(target_arch = "x86_64") {
            assert_eq!(up.runtimes(), [RT4]);
        }
        let p = up.plan(RT4);
        assert_eq!(p.program, dir.path().join("umu-run").to_str().unwrap());
        assert_eq!(p.resolve_args(None), ["true"]);
        assert_eq!(p.env["UMU_NO_PROTON"], "1");
        assert_eq!(p.env["RUNTIMEPATH"], "steamrt4");
        assert_eq!(p.env["UMU_ZENITY"], "", "never a download window");
        assert!(!p.env.contains_key("UMU_RUNTIME_UPDATE"));

        assert!(up.due(RT4, SystemTime::now(), None));
        write(&up.log(RT4), "INFO: Updating steamrt4 to latest...\nINFO: steamrt4 is up to date\n");
        assert!(up.succeeded(RT4, 0));
        assert!(!up.succeeded(RT4, 1));
        up.mark_checked(RT4).unwrap();
        assert!(!up.due(RT4, SystemTime::now(), None));
        write(&up.log(RT4), "INFO: Downloading steamrt4 (latest), please wait...\nERROR: Aborting steamrt install due to network error\n");
        assert!(!up.succeeded(RT4, 0), "umu exits 0 after a failed update when the old runtime works");
    }
}
