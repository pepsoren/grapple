//! Looking at running processes through `/proc` (read-only), and signalling them.
//!
//! The scanner is cheap by design: a poll lists `/proc`, reads one small `stat` file per process, and reads
//! `environ` (and `cmdline` for `reaper`) only for processes it has not seen before. Everything is cached by
//! `(pid, starttime)`, so a reused pid is treated as a new process.
//!
//! On Windows the same scanner runs on the live process table ([`crate::winproc`]): a Toolhelp32 snapshot instead of
//! listing `/proc`, and tags from the executable's folder instead of the environment. A fake `/proc` folder
//! ([`ProcFs::at`]) works on every platform (tests).

use grapple_core::{GameId, Store};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What we remember about a process from its environment / command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    /// Steam game id from `SteamAppId`, `SteamGameId` or `reaper SteamLaunch AppId=<id>` (digits only, not "0").
    /// Only for games the Steam client runs: umu also sets `SteamAppId` (from its `umu-<id>` GAMEID, e.g.
    /// Rocket League on Epic is `umu-252950`), so a process started by umu, Heroic or Grapple's own Epic/manual
    /// launch never gets one from its environment.
    pub steam_app: Option<String>,
    /// `GRAPPLE_GAME=<game id>`: set by Grapple on the Epic and manual games it starts.
    pub grapple_game: Option<String>,
    /// `HEROIC_APP_NAME=<app_name>`: set by Heroic on games it starts (so we can see those too).
    pub heroic_app: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub starttime: u64,
    /// Kernel `comm` (max 15 chars).
    pub comm: String,
    /// Single-letter state from `stat` (`Z` = zombie).
    pub state: char,
    pub tags: Tags,
}

impl ProcInfo {
    pub fn alive(&self) -> bool {
        !matches!(self.state, 'Z' | 'X' | 'x')
    }
}

/// Processes that wine/Proton/Steam leave around for a few seconds after a game ends. They never keep a
/// session open on their own, and they carry the game's environment so they would otherwise count.
pub fn is_helper(comm: &str) -> bool {
    const HELPERS: &[&str] = &[
        "wineserver",
        "services.exe",
        "winedevice",
        "plugplay.exe",
        "svchost.exe",
        "explorer.exe",
        "rpcss.exe",
        "conhost.exe",
        "winemenubuilder",
        "tabtip.exe",
        "pv-adverb",
        "gameoverlayui",
        "steam-runtime-l",
        "srt-logger",
    ];
    HELPERS.iter().any(|h| comm.starts_with(h)) || (cfg!(windows) && WINDOWS_HELPERS.iter().any(|h| comm.eq_ignore_ascii_case(h)))
}

/// Windows programs that sit next to a game (crash reporters, the console host) and may outlive it briefly.
const WINDOWS_HELPERS: &[&str] = &["conhost.exe", "UnityCrashHandler64.exe", "UnityCrashHandler32.exe", "CrashReportClient.exe", "werfault.exe"];

/// Processes Grapple must never signal. Steam itself and its helpers: closing Steam is `steam -shutdown`.
pub fn is_protected(comm: &str) -> bool {
    matches!(comm, "steam" | "steamwebhelper" | "steam.sh" | "systemd" | "init")
        || comm.starts_with("steam-runtime")
        || comm.starts_with("srt-")
        || (cfg!(windows) && WINDOWS_PROTECTED.iter().any(|p| comm.eq_ignore_ascii_case(p)))
}

/// Windows executables that are Steam's or Windows' own (Windows names keep their case and extension).
const WINDOWS_PROTECTED: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "steamservice.exe",
    "steamerrorreporter.exe",
    "explorer.exe",
    "csrss.exe",
    "winlogon.exe",
    "services.exe",
    "svchost.exe",
];

#[derive(Debug, Clone)]
pub struct ProcFs {
    root: PathBuf,
    /// The live process table (Windows has no `/proc`). `None` for a fake `/proc` folder.
    #[cfg(windows)]
    live: Option<std::sync::Arc<crate::winproc::Live>>,
}

#[cfg(not(windows))]
impl Default for ProcFs {
    fn default() -> Self {
        ProcFs { root: PathBuf::from("/proc") }
    }
}

#[cfg(windows)]
impl Default for ProcFs {
    fn default() -> Self {
        ProcFs { root: PathBuf::new(), live: Some(Default::default()) }
    }
}

impl ProcFs {
    /// A fake `/proc` for tests.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        ProcFs {
            root: root.into(),
            #[cfg(windows)]
            live: None,
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Which games own which executables (Windows, where there is no environment to read; see [`crate::winproc`]).
    /// Returns true when that changed, so the scanner reads the processes again. Elsewhere it does nothing.
    pub fn set_owners(&self, owners: Vec<crate::winproc::Owner>) -> bool {
        #[cfg(windows)]
        if let Some(l) = &self.live {
            return l.set_owners(owners);
        }
        let _ = owners;
        false
    }

    pub fn pids(&self) -> Vec<u32> {
        #[cfg(windows)]
        if let Some(l) = &self.live {
            return l.pids();
        }
        let Ok(rd) = std::fs::read_dir(&self.root) else { return Vec::new() };
        rd.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok()).collect()
    }

    /// Parse `/proc/<pid>/stat`. `comm` may contain spaces and parentheses, so split at the *last* `)`.
    pub fn stat(&self, pid: u32) -> Option<(String, char, u32, u64)> {
        #[cfg(windows)]
        if let Some(l) = &self.live {
            return l.stat(pid);
        }
        let s = std::fs::read_to_string(self.root.join(pid.to_string()).join("stat")).ok()?;
        parse_stat(&s)
    }

    fn environ_tags(&self, pid: u32, tags: &mut Tags) {
        let Ok(bytes) = std::fs::read(self.root.join(pid.to_string()).join("environ")) else { return };
        let mut steam_app = None;
        let mut not_steam = false;
        for kv in bytes.split(|b| *b == 0) {
            let Some(eq) = kv.iter().position(|b| *b == b'=') else { continue };
            let (k, v) = (&kv[..eq], &kv[eq + 1..]);
            match k {
                b"SteamAppId" | b"SteamGameId" => {
                    if let Some(id) = steam_id(v) {
                        // SteamAppId is the real appid; SteamGameId can differ for shortcuts. First one wins,
                        // SteamAppId comes first in practice.
                        steam_app.get_or_insert(id);
                    }
                }
                // umu-run sets SteamAppId/SteamGameId itself, outside Steam.
                b"UMU_ID" => not_steam = true,
                b"GRAPPLE_GAME" => tags.grapple_game = String::from_utf8(v.to_vec()).ok(),
                b"HEROIC_APP_NAME" => tags.heroic_app = String::from_utf8(v.to_vec()).ok(),
                _ => {}
            }
        }
        let other_launcher = tags.heroic_app.is_some()
            || tags.grapple_game.as_deref().is_some_and(|g| !g.starts_with("steam:"));
        if !not_steam && !other_launcher {
            tags.steam_app = steam_app;
        }
    }

    fn reaper_tag(&self, pid: u32) -> Option<String> {
        let bytes = std::fs::read(self.root.join(pid.to_string()).join("cmdline")).ok()?;
        let mut args = bytes.split(|b| *b == 0);
        // reaper SteamLaunch AppId=<id> -- ...
        let mut saw_launch = false;
        for a in args.by_ref() {
            if a == b"SteamLaunch" {
                saw_launch = true;
            } else if saw_launch {
                if let Some(v) = a.strip_prefix(b"AppId=") {
                    return steam_id(v);
                }
            }
            if a == b"--" {
                break;
            }
        }
        None
    }

    /// The first word of the command line (for Wine processes, the Windows path of the `.exe`; on Windows the
    /// executable's path).
    pub fn argv0(&self, pid: u32) -> Option<String> {
        #[cfg(windows)]
        if self.live.is_some() {
            return crate::winproc::live::exe_path(pid).map(|p| p.to_string_lossy().into_owned());
        }
        let bytes = std::fs::read(self.root.join(pid.to_string()).join("cmdline")).ok()?;
        let first = bytes.split(|b| *b == 0).next()?;
        (!first.is_empty()).then(|| String::from_utf8_lossy(first).into_owned())
    }

    /// Read one process fully (stat + tags). `None` if it vanished.
    pub fn read(&self, pid: u32) -> Option<ProcInfo> {
        #[cfg(windows)]
        if let Some(l) = &self.live {
            return l.read(pid);
        }
        let (comm, state, ppid, starttime) = self.stat(pid)?;
        let mut tags = Tags::default();
        self.environ_tags(pid, &mut tags);
        if comm == "reaper" {
            if let Some(id) = self.reaper_tag(pid) {
                tags.steam_app = Some(id);
            }
        }
        Some(ProcInfo { pid, ppid, starttime, comm, state, tags })
    }
}

fn steam_id(v: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(v).ok()?.trim();
    (!s.is_empty() && s != "0" && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_string())
}

pub(crate) fn parse_stat(s: &str) -> Option<(String, char, u32, u64)> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    // rest[0]=state rest[1]=ppid ... rest[19]=starttime
    let state = rest.first()?.chars().next()?;
    let ppid = rest.get(1)?.parse().ok()?;
    let starttime = rest.get(19)?.parse().ok()?;
    Some((comm, state, ppid, starttime))
}

/// Cached view of the process table.
pub struct Scanner {
    fs: ProcFs,
    procs: HashMap<u32, ProcInfo>,
    /// Processes seen by any `refresh` that the monitor has not looked at yet. `refresh` is also called by
    /// launches and `stop`, which must not swallow what the monitor needs to see.
    pending_new: Vec<u32>,
}

/// What changed in one refresh.
#[derive(Debug, Default)]
pub struct Delta {
    pub new: Vec<u32>,
    pub gone: Vec<u32>,
}

impl Scanner {
    pub fn new(fs: ProcFs) -> Self {
        Scanner { fs, procs: HashMap::new(), pending_new: Vec::new() }
    }
    pub fn fs(&self) -> &ProcFs {
        &self.fs
    }
    pub fn get(&self, pid: u32) -> Option<&ProcInfo> {
        self.procs.get(&pid)
    }
    pub fn all(&self) -> impl Iterator<Item = &ProcInfo> {
        self.procs.values()
    }

    /// List `/proc`, reading `stat` for everything (to notice exits and pid reuse) but `environ` only for
    /// processes we have not seen.
    pub fn refresh(&mut self) -> Delta {
        let mut delta = Delta::default();
        let listed: HashSet<u32> = self.fs.pids().into_iter().collect();
        let known: Vec<u32> = self.procs.keys().copied().collect();
        for pid in known {
            if !listed.contains(&pid) {
                self.procs.remove(&pid);
                delta.gone.push(pid);
                continue;
            }
            // Keep ppid/state fresh, notice a reused pid.
            match self.fs.stat(pid) {
                Some((comm, state, ppid, starttime)) => {
                    let p = self.procs.get_mut(&pid).unwrap();
                    // A changed comm means the process exec'd since we first looked (fork, then exec): its
                    // environment is a different one now, read it again.
                    if p.starttime != starttime || p.comm != comm {
                        let reused = p.starttime != starttime;
                        self.procs.remove(&pid);
                        if reused {
                            delta.gone.push(pid);
                        }
                        if let Some(info) = self.fs.read(pid) {
                            self.procs.insert(pid, info);
                            delta.new.push(pid);
                        }
                    } else {
                        p.state = state;
                        p.ppid = ppid;
                    }
                }
                None => {
                    self.procs.remove(&pid);
                    delta.gone.push(pid);
                }
            }
        }
        for pid in listed {
            if self.procs.contains_key(&pid) {
                continue;
            }
            if let Some(info) = self.fs.read(pid) {
                self.procs.insert(pid, info);
                delta.new.push(pid);
            }
        }
        delta.new.sort_unstable();
        self.pending_new.extend(delta.new.iter().copied());
        delta
    }

    /// Forget every process, so the next `refresh` reads them all again and reports them as new (after the owners
    /// changed on Windows, the tags of running processes may have too).
    pub fn forget(&mut self) {
        self.procs.clear();
    }

    /// Take everything new since the last call (see `pending_new`).
    pub fn take_pending(&mut self) -> Vec<u32> {
        let mut v = std::mem::take(&mut self.pending_new);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Is the process `pid` a live descendant of (or equal to) `root`?
    pub fn descends_from(&self, mut pid: u32, root: u32) -> bool {
        for _ in 0..64 {
            if pid == root {
                return true;
            }
            match self.procs.get(&pid) {
                Some(p) if p.ppid > 1 => pid = p.ppid,
                _ => return false,
            }
        }
        false
    }
}

/// How to recognise a game's processes.
#[derive(Debug, Clone)]
pub struct Matcher {
    pub game: GameId,
    /// For games Grapple spawned itself: the root process and its start time (guards against pid reuse).
    pub root: Option<(u32, u64)>,
}

impl Matcher {
    pub fn new(game: GameId) -> Self {
        Matcher { game, root: None }
    }

    /// Does this process belong to the game by its own markers (not by tree position)?
    pub fn tag_match(&self, p: &ProcInfo) -> bool {
        let id = self.game.store_id();
        match store_of(&self.game) {
            Store::Steam => p.tags.steam_app.as_deref() == Some(id),
            Store::Epic => p.tags.grapple_game.as_deref() == Some(self.game.0.as_str()) || p.tags.heroic_app.as_deref() == Some(id),
            Store::Manual => p.tags.grapple_game.as_deref() == Some(self.game.0.as_str()),
        }
    }

    /// Is the root still the same live process?
    pub fn root_alive(&self, scan: &Scanner) -> bool {
        match self.root {
            Some((pid, start)) => scan.get(pid).is_some_and(|p| p.starttime == start && p.alive()),
            None => false,
        }
    }

    /// Decide membership of the processes in `candidates` given the already-known `members`.
    /// Iterates to a fixpoint so a child seen in the same poll as its parent is found.
    pub fn grow(&self, scan: &Scanner, members: &mut HashSet<u32>, candidates: &[u32]) {
        let mut pending: Vec<u32> = candidates.to_vec();
        loop {
            let before = pending.len();
            pending.retain(|pid| {
                let Some(p) = scan.get(*pid) else { return false };
                let by_tree = match self.root {
                    Some((root, start)) => {
                        (p.pid == root && p.starttime == start) || (members.contains(&p.ppid) && parent_not_newer(scan, p))
                    }
                    None => false,
                };
                if by_tree || self.tag_match(p) {
                    members.insert(*pid);
                    false
                } else {
                    true
                }
            });
            if pending.len() == before || pending.is_empty() {
                break;
            }
        }
    }

    /// Drop members that exited.
    pub fn prune(&self, scan: &Scanner, members: &mut HashSet<u32>) {
        members.retain(|pid| scan.get(*pid).is_some_and(|p| p.alive()));
    }

    /// All current processes of the game, found from scratch (used when adopting a game after a restart).
    pub fn find_all(&self, scan: &Scanner) -> HashSet<u32> {
        let mut m = HashSet::new();
        let all: Vec<u32> = scan.all().map(|p| p.pid).collect();
        self.grow(scan, &mut m, &all);
        m
    }
}

/// Windows keeps a process's parent pid after the parent exits, and pids are reused, so a "parent" that started
/// after its child is someone else. (Linux reparents orphans, so the check is not needed there.)
fn parent_not_newer(scan: &Scanner, p: &ProcInfo) -> bool {
    !cfg!(windows) || scan.get(p.ppid).is_none_or(|parent| parent.starttime <= p.starttime)
}

pub fn store_of(id: &GameId) -> Store {
    match id.0.split_once(':').map(|(s, _)| s) {
        Some("steam") => Store::Steam,
        Some("epic") => Store::Epic,
        _ => Store::Manual,
    }
}

/// Members that count: alive and not a leftover helper.
pub fn significant(scan: &Scanner, members: &HashSet<u32>) -> Vec<u32> {
    members
        .iter()
        .copied()
        .filter(|pid| scan.get(*pid).is_some_and(|p| p.alive() && !is_helper(&p.comm) && !is_protected(&p.comm)))
        .collect()
}

/// SIGTERM `pids`, skipping anything protected, pid 1 and ourselves. Returns how many were signalled.
#[cfg(not(windows))]
pub fn terminate(scan: &Scanner, pids: &HashSet<u32>, never: &HashSet<u32>) -> usize {
    let mut n = 0;
    for pid in targets(scan, pids, never) {
        // SAFETY: plain kill(2); the pid came from /proc moments ago.
        if unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0 {
            n += 1;
        }
    }
    n
}

/// What Windows has instead of SIGTERM: `WM_CLOSE` to the windows of `pids` (skipping anything protected and
/// ourselves), then, after [`crate::winproc::live::CLOSE_GRACE`], `TerminateProcess` for those of them that are
/// still running (the same process: start times are compared). Steam is never touched. Returns how many processes
/// were asked.
#[cfg(windows)]
pub fn terminate(scan: &Scanner, pids: &HashSet<u32>, never: &HashSet<u32>) -> usize {
    use crate::winproc::live;
    let chosen = targets(scan, pids, never);
    if chosen.is_empty() || scan.fs().live.is_none() {
        // A fake /proc (tests) has nothing to signal.
        return 0;
    }
    let set: HashSet<u32> = chosen.iter().copied().collect();
    let with_window = live::close_windows(&set);
    tracing::info!("asked {} of {} processes to close", with_window.len(), set.len());
    let starts: Vec<(u32, u64)> = chosen.iter().filter_map(|pid| scan.get(*pid).map(|p| (*pid, p.starttime))).collect();
    std::thread::spawn(move || {
        std::thread::sleep(live::CLOSE_GRACE);
        for (pid, start) in starts {
            if live::terminate_if_same(pid, start) {
                tracing::info!("ended pid {pid}: still running {} s after it was asked to close", live::CLOSE_GRACE.as_secs());
            }
        }
    });
    set.len()
}

/// The processes of `pids` that may be stopped: alive, not protected, not pid 1, not ourselves, not in `never`.
fn targets(scan: &Scanner, pids: &HashSet<u32>, never: &HashSet<u32>) -> Vec<u32> {
    let me = std::process::id();
    let mut out = Vec::new();
    for pid in pids {
        if *pid <= 1 || *pid == me || never.contains(pid) {
            continue;
        }
        match scan.get(*pid) {
            Some(p) if is_protected(&p.comm) || !p.alive() => continue,
            None => continue,
            _ => {}
        }
        out.push(*pid);
    }
    out
}

/// The pid in a Steam pid file, if that process is alive and called `steam`.
#[cfg(not(windows))]
pub fn steam_pid(fs: &ProcFs, pid_file: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()?;
    let (comm, state, _, _) = fs.stat(pid)?;
    (comm == "steam" && !matches!(state, 'Z' | 'X')).then_some(pid)
}

/// The running Steam client: the pid Steam writes to `HKCU\Software\Valve\Steam\ActiveProcess\pid` (0 once it
/// exits), if that process is alive and is `steam.exe`. `pid_file` is not used (Steam on Windows has none).
#[cfg(windows)]
pub fn steam_pid(fs: &ProcFs, pid_file: &Path) -> Option<u32> {
    let _ = pid_file;
    let pid = grapple_core::winreg::read_dword(grapple_core::winreg::Hive::CurrentUser, crate::winproc::STEAM_ACTIVE_KEY, "pid")?;
    if pid == 0 {
        return None;
    }
    let (comm, _, _, _) = fs.stat(pid)?;
    comm.eq_ignore_ascii_case("steam.exe").then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_awkward_comm() {
        let s = "123 (we ird) name) S 45 123 123 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
        let (comm, state, ppid, start) = parse_stat(s).unwrap();
        assert_eq!(comm, "we ird) name");
        assert_eq!(state, 'S');
        assert_eq!(ppid, 45);
        assert_eq!(start, 987654);
    }

    #[test]
    fn steam_ids() {
        assert_eq!(steam_id(b"730"), Some("730".into()));
        assert_eq!(steam_id(b"0"), None);
        assert_eq!(steam_id(b"heroic-Sugar"), None);
    }

    // ---- a fake /proc

    fn fake_proc(dir: &Path, pid: u32, comm: &str, ppid: u32, start: u64, env: &[(&str, &str)], cmdline: &[&str]) {
        let d = dir.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("stat"), format!("{pid} ({comm}) S {ppid} 0 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0\n")).unwrap();
        let mut e = Vec::new();
        for (k, v) in env {
            e.extend_from_slice(format!("{k}={v}").as_bytes());
            e.push(0);
        }
        std::fs::write(d.join("environ"), e).unwrap();
        let mut c = Vec::new();
        for a in cmdline {
            c.extend_from_slice(a.as_bytes());
            c.push(0);
        }
        std::fs::write(d.join("cmdline"), c).unwrap();
    }

    #[test]
    fn steam_game_found_by_env_and_by_reaper_parent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fake_proc(p, 10, "steam", 1, 100, &[], &["steam"]);
        fake_proc(p, 20, "reaper", 10, 200, &[], &["reaper", "SteamLaunch", "AppId=730", "--", "/x/cs2"]);
        fake_proc(p, 21, "cs2", 20, 201, &[("SteamAppId", "730"), ("HOME", "/h")], &["cs2"]);
        fake_proc(p, 22, "wineserver", 20, 202, &[("SteamAppId", "730")], &["wineserver"]);
        fake_proc(p, 30, "other", 10, 300, &[("SteamAppId", "570")], &[]);
        fake_proc(p, 40, "heroic-game", 1, 400, &[("HEROIC_APP_NAME", "Sugar")], &[]);
        let mut scan = Scanner::new(ProcFs::at(p));
        let delta = scan.refresh();
        assert_eq!(delta.new.len(), 6);

        let m = Matcher::new(GameId::new(Store::Steam, "730"));
        let found = m.find_all(&scan);
        assert_eq!(found, HashSet::from([20, 21, 22]));
        // wineserver lingers but does not keep a session alive on its own
        assert_eq!(significant(&scan, &found).len(), 2);
        let only_helper: HashSet<u32> = HashSet::from([22]);
        assert!(significant(&scan, &only_helper).is_empty());

        let epic = Matcher::new(GameId::new(Store::Epic, "Sugar"));
        assert_eq!(epic.find_all(&scan), HashSet::from([40]), "games started by Heroic are recognised");
        assert!(Matcher::new(GameId::new(Store::Steam, "999")).find_all(&scan).is_empty());
    }

    #[test]
    fn games_run_by_umu_heroic_or_grapple_are_not_steam_games() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        // what umu-run gives Rocket League on Epic (GAMEID=umu-252950), started by Grapple and by Heroic
        let umu = [("SteamAppId", "252950"), ("SteamGameId", "252950"), ("UMU_ID", "umu-252950"), ("STORE", "egs")];
        fake_proc(p, 10, "RocketLeague.ex", 1, 100, &[&umu[..], &[("GRAPPLE_GAME", "epic:Sugar")]].concat(), &[]);
        fake_proc(p, 11, "RocketLeague.ex", 1, 110, &[&umu[..], &[("HEROIC_APP_NAME", "Sugar")]].concat(), &[]);
        fake_proc(p, 12, "game.exe", 1, 120, &umu, &[]);
        fake_proc(p, 13, "game.exe", 1, 130, &[("SteamAppId", "570"), ("GRAPPLE_GAME", "manual:x")], &[]);
        // a real Steam game keeps its id
        fake_proc(p, 20, "cs2", 1, 200, &[("SteamAppId", "730")], &[]);
        let mut scan = Scanner::new(ProcFs::at(p));
        scan.refresh();
        for pid in [10, 11, 12, 13] {
            assert_eq!(scan.get(pid).unwrap().tags.steam_app, None, "pid {pid}");
        }
        assert_eq!(scan.get(20).unwrap().tags.steam_app.as_deref(), Some("730"));
        assert!(Matcher::new(GameId::new(Store::Steam, "252950")).find_all(&scan).is_empty());
        assert_eq!(Matcher::new(GameId::new(Store::Epic, "Sugar")).find_all(&scan), HashSet::from([10, 11]));
    }

    #[test]
    fn tree_membership_follows_children_and_survives_reparenting() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fake_proc(p, 100, "umu-run", 1, 1000, &[], &[]);
        fake_proc(p, 101, "proton", 100, 1001, &[], &[]);
        fake_proc(p, 102, "game.exe", 101, 1002, &[], &[]);
        fake_proc(p, 200, "unrelated", 1, 2000, &[], &[]);
        let mut scan = Scanner::new(ProcFs::at(p));
        scan.refresh();
        let m = Matcher { game: GameId::new(Store::Manual, "x"), root: Some((100, 1000)) };
        let mut members = HashSet::new();
        let pending = scan.take_pending();
        m.grow(&scan, &mut members, &pending);
        assert_eq!(members, HashSet::from([100, 101, 102]));

        // the middle process exits and the game is reparented to init: it stays a member
        std::fs::remove_dir_all(p.join("101")).unwrap();
        std::fs::write(p.join("102").join("stat"), "102 (game.exe) S 1 0 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1002 0 0\n").unwrap();
        scan.refresh();
        m.prune(&scan, &mut members);
        assert_eq!(members, HashSet::from([100, 102]));

        // a pid reused by an unrelated process is not the root any more
        std::fs::write(p.join("100").join("stat"), "100 (other) S 1 0 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9999 0 0\n").unwrap();
        let delta = scan.refresh();
        assert!(delta.gone.contains(&100) && delta.new.contains(&100));
        assert!(!m.root_alive(&scan));
    }

    #[test]
    fn never_signals_steam_or_pid_one() {
        assert!(is_protected("steam") && is_protected("steamwebhelper"));
        assert!(!is_protected("cs2"));
        let dir = tempfile::tempdir().unwrap();
        fake_proc(dir.path(), 4_000_000, "steam", 1, 1, &[], &[]);
        let mut scan = Scanner::new(ProcFs::at(dir.path()));
        scan.refresh();
        // not signalled: protected comm (and the pid is not a real process anyway)
        assert_eq!(terminate(&scan, &HashSet::from([4_000_000, 1]), &HashSet::new()), 0);
    }
}
