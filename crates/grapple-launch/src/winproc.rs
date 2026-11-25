//! Processes on Windows, where another process's environment cannot be read (no `/proc/<pid>/environ`; reading a
//! PEB is undocumented and anti-cheat protected games refuse it). Games are recognised by **where their executable
//! is** instead:
//!
//! * a Steam game: an executable under its install folder (`steamapps\common\<installdir>` from the app manifest);
//! * an Epic game: under its install folder (legendary's `install_path` or the Epic Games Launcher's
//!   `InstallLocation`), which also catches Epic games started by the Epic launcher or Heroic;
//! * a hand-added game: its executable exactly, or its folder when that folder is clearly the game's own (deep
//!   enough and outside the Windows folder, so `C:\Windows\System32\cmd.exe` never claims every system process).
//!
//! When two Steam apps share an install folder (a game and its playtest), Steam's `RunningAppID` registry value
//! picks the one Steam is running. Games grapple starts are also followed down their process tree (as on Linux).
//!
//! The decision logic here is plain Rust and tested on every platform; the system calls are in [`live`].

use crate::manual::ManualEntry;
use crate::proc::Tags;
use grapple_core::{Game, GameId, Store};
use std::collections::HashMap;

/// What claims a process for a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerPath {
    /// Every executable under this folder.
    Dir(String),
    /// Exactly this executable.
    Exe(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub game: GameId,
    pub path: OwnerPath,
}

/// A path in the form comparisons use: lowercase (Windows paths are case-insensitive), `\` separators, no
/// trailing separator, no `\\?\` prefix.
pub fn norm(p: &str) -> String {
    let s = p.trim().replace('/', "\\");
    let s = s.strip_prefix("\\\\?\\").unwrap_or(&s);
    s.trim_end_matches('\\').to_lowercase()
}

/// Folder levels below the drive (`c:\games\foo` has two).
fn depth(normed: &str) -> usize {
    normed.split('\\').filter(|c| !c.is_empty() && !(c.len() == 2 && c.ends_with(':'))).count()
}

/// Can every executable under `dir` be counted as one game's? Not a drive or top-level folder (`C:\Games`), not
/// the Windows folder.
pub fn game_folder(dir: &str, windows_dir: &str) -> bool {
    let d = norm(dir);
    let w = norm(windows_dir);
    depth(&d) >= 2 && !(d == w || d.starts_with(&format!("{w}\\")))
}

/// Who owns which paths: installed Steam and Epic games by folder, hand-added games by executable (or folder, see
/// [`game_folder`]). `windows_dir` is `%SystemRoot%` (`C:\Windows`).
pub fn owners(games: &[Game], manual: &HashMap<GameId, ManualEntry>, windows_dir: &str) -> Vec<Owner> {
    let mut out = Vec::new();
    for g in games {
        match g.store {
            Store::Steam | Store::Epic => {
                if let Some(dir) = g.install_dir.as_deref().filter(|_| g.installed) {
                    let d = dir.to_string_lossy();
                    if game_folder(&d, windows_dir) {
                        out.push(Owner { game: g.id.clone(), path: OwnerPath::Dir(norm(&d)) });
                    }
                }
            }
            Store::Manual => {
                let Some(e) = manual.get(&g.id) else { continue };
                if !e.exe.contains(['\\', '/']) {
                    continue;
                }
                let exe = norm(&e.exe);
                let dir = exe.rsplit_once('\\').map(|(d, _)| d.to_string()).unwrap_or_default();
                let path = if game_folder(&dir, windows_dir) { OwnerPath::Dir(dir) } else { OwnerPath::Exe(exe) };
                out.push(Owner { game: g.id.clone(), path });
            }
        }
    }
    out
}

/// The game a process with executable `exe` belongs to. The most specific owner wins (an exact executable, then
/// the deepest folder); among Steam apps sharing that folder, `running_steam_app` (Steam's `RunningAppID`) decides.
pub fn owner_of(exe: &str, owners: &[Owner], running_steam_app: Option<u32>) -> Option<GameId> {
    let e = norm(exe);
    let mut best: Vec<(&GameId, usize)> = Vec::new();
    for o in owners {
        let score = match &o.path {
            OwnerPath::Exe(x) if *x == e => usize::MAX,
            OwnerPath::Dir(d) if e.len() > d.len() && e.starts_with(d.as_str()) && e.as_bytes()[d.len()] == b'\\' => d.len(),
            _ => continue,
        };
        match best.first().map(|(_, s)| *s) {
            Some(s) if s > score => {}
            Some(s) if s == score => best.push((&o.game, score)),
            _ => best = vec![(&o.game, score)],
        }
    }
    if best.len() > 1 {
        if let Some(running) = running_steam_app.filter(|id| *id != 0) {
            let want = GameId::new(Store::Steam, &running.to_string());
            if let Some((g, _)) = best.iter().find(|(g, _)| **g == want) {
                return Some((*g).clone());
            }
        }
        best.sort_by(|a, b| a.0.cmp(b.0));
    }
    best.first().map(|(g, _)| (*g).clone())
}

/// The tags a process gets from its owner (what `/proc/<pid>/environ` gives on Linux).
pub fn tags_for(owner: Option<GameId>) -> Tags {
    let mut t = Tags::default();
    match owner {
        Some(g) if crate::proc::store_of(&g) == Store::Steam => t.steam_app = Some(g.store_id().to_string()),
        Some(g) => t.grapple_game = Some(g.0),
        None => {}
    }
    t
}

/// Has Steam come up far enough to take `-applaunch`? Its `ActiveProcess` registry key names a live `steam.exe`
/// (`pid`) and a signed-in account (`ActiveUser`, 0 while the login window shows).
pub fn steam_ready(pid_is_live_steam: bool, active_user: Option<u32>) -> bool {
    pid_is_live_steam && active_user.is_some_and(|u| u != 0)
}

/// `%SystemRoot%`, for [`game_folder`].
pub fn windows_dir() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
}

/// Steam's registry key (per user).
pub const STEAM_KEY: &str = "Software\\Valve\\Steam";
pub const STEAM_ACTIVE_KEY: &str = "Software\\Valve\\Steam\\ActiveProcess";

/// The game Steam says it is running (`RunningAppID`, 0 for none).
#[cfg(windows)]
pub fn running_steam_app() -> Option<u32> {
    grapple_core::winreg::read_dword(grapple_core::winreg::Hive::CurrentUser, STEAM_KEY, "RunningAppID").filter(|id| *id != 0)
}

#[cfg(windows)]
pub use live::Live;

#[cfg(windows)]
pub mod live {
    //! The system calls: a Toolhelp32 snapshot for pids and parents, `GetProcessTimes` for the start time (tells a
    //! reused pid apart), `QueryFullProcessImageNameW` for the executable, `WM_CLOSE` and `TerminateProcess` to stop.
    use super::{Owner, owner_of, running_steam_app, tags_for};
    use crate::proc::ProcInfo;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::{Mutex, RwLock};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, TRUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE};

    /// How long a game gets to close after `WM_CLOSE` before it is terminated.
    pub const CLOSE_GRACE: Duration = Duration::from_secs(10);

    #[derive(Debug, Clone)]
    struct Row {
        ppid: u32,
        name: String,
        start: u64,
    }

    /// The live process table of this machine.
    #[derive(Debug, Default)]
    pub struct Live {
        rows: Mutex<HashMap<u32, Row>>,
        owners: RwLock<Vec<Owner>>,
    }

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle this module opened and nobody else closes.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> Option<Handle> {
        // SAFETY: plain OpenProcess; a null result means no access or no such process.
        let h = unsafe { OpenProcess(access, 0, pid) };
        (!h.is_null()).then_some(Handle(h))
    }

    fn wide_str(units: &[u16]) -> String {
        grapple_core::winreg::string_from_wide(units)
    }

    /// The process's creation time (100 ns ticks since 1601), 0 when it cannot be opened (protected processes).
    pub fn creation_time(pid: u32) -> u64 {
        let Some(h) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else { return 0 };
        let mut t = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        // SAFETY: four FILETIMEs for creation, exit, kernel and user time.
        let ok = unsafe { GetProcessTimes(h.0, &mut t[0], &mut t[1], &mut t[2], &mut t[3]) };
        if ok == 0 {
            return 0;
        }
        (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime)
    }

    /// Full path of the process's executable.
    pub fn exe_path(pid: u32) -> Option<PathBuf> {
        let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` UTF-16 units.
        let ok = unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) };
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }

    impl Live {
        pub fn set_owners(&self, owners: Vec<Owner>) -> bool {
            let mut cur = self.owners.write().unwrap_or_else(|e| e.into_inner());
            if *cur == owners {
                return false;
            }
            *cur = owners;
            true
        }

        /// Take a snapshot of all processes (pids, parents, names, start times).
        pub fn pids(&self) -> Vec<u32> {
            let mut rows = HashMap::new();
            // SAFETY: a process snapshot; the handle is closed by `Handle`.
            let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snap == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let snap = Handle(snap);
            // SAFETY: PROCESSENTRY32W is plain data; dwSize is set as the API requires.
            let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: `e` is a valid, sized entry for the snapshot's iteration.
            let mut more = unsafe { Process32FirstW(snap.0, &mut e) } == TRUE;
            while more {
                let pid = e.th32ProcessID;
                // 0 is the idle process and 4 the kernel: never games.
                if pid > 4 {
                    rows.insert(pid, Row { ppid: e.th32ParentProcessID, name: wide_str(&e.szExeFile), start: creation_time(pid) });
                }
                // SAFETY: as above.
                more = unsafe { Process32NextW(snap.0, &mut e) } == TRUE;
            }
            let pids = rows.keys().copied().collect();
            *self.rows.lock().unwrap_or_else(|e| e.into_inner()) = rows;
            pids
        }

        /// `(name, state, ppid, start)` like `/proc/<pid>/stat`. A process newer than the last snapshot (one grapple
        /// just spawned) is asked directly; its parent is then unknown (0).
        pub fn stat(&self, pid: u32) -> Option<(String, char, u32, u64)> {
            if let Some(r) = self.rows.lock().unwrap_or_else(|e| e.into_inner()).get(&pid) {
                return Some((r.name.clone(), 'R', r.ppid, r.start));
            }
            let exe = exe_path(pid)?;
            let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            Some((name, 'R', 0, creation_time(pid)))
        }

        pub fn read(&self, pid: u32) -> Option<ProcInfo> {
            let (comm, state, ppid, starttime) = self.stat(pid)?;
            let tags = match exe_path(pid) {
                Some(exe) => {
                    let owners = self.owners.read().unwrap_or_else(|e| e.into_inner());
                    let owner = owner_of(&exe.to_string_lossy(), &owners, running_steam_app());
                    tags_for(owner)
                }
                None => Default::default(),
            };
            Some(ProcInfo { pid, ppid, starttime, comm, state, tags })
        }
    }

    /// Ask the processes politely: `WM_CLOSE` to each visible top-level window they own (what clicking the window's
    /// close button does). Returns the pids that had a window.
    pub fn close_windows(pids: &HashSet<u32>) -> HashSet<u32> {
        struct Ctx<'a> {
            pids: &'a HashSet<u32>,
            hit: HashSet<u32>,
        }
        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> i32 {
            // SAFETY: `lparam` is the `Ctx` passed to EnumWindows below, alive for the whole call.
            let ctx = unsafe { &mut *(lparam as *mut Ctx) };
            let mut pid = 0u32;
            // SAFETY: plain window queries on a window EnumWindows handed us.
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if ctx.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd) } != 0 {
                // SAFETY: posting a message never blocks on the target.
                unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
                ctx.hit.insert(pid);
            }
            TRUE
        }
        let mut ctx = Ctx { pids, hit: HashSet::new() };
        // SAFETY: `each` only uses `ctx` during this call.
        unsafe { EnumWindows(Some(each), &mut ctx as *mut Ctx as LPARAM) };
        ctx.hit
    }

    /// End the process `pid` if it is still the one that started at `start` and is not protected.
    pub fn terminate_if_same(pid: u32, start: u64) -> bool {
        if creation_time(pid) != start {
            return false;
        }
        if exe_path(pid).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).is_some_and(|n| crate::proc::is_protected(&n)) {
            return false;
        }
        let Some(h) = open(pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION) else { return false };
        // SAFETY: a handle opened with PROCESS_TERMINATE.
        unsafe { TerminateProcess(h.0, 1) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapple_core::Art;
    use std::path::PathBuf;

    fn game(store: Store, id: &str, dir: Option<&str>) -> Game {
        Game {
            id: GameId::new(store, id),
            store,
            name: id.into(),
            sort_name: id.into(),
            installed: dir.is_some(),
            install_dir: dir.map(PathBuf::from),
            size_bytes: None,
            art: Art::default(),
            store_playtime_min: 0,
            last_played: None,
            status: None,
        }
    }

    #[test]
    fn paths_compare_like_windows() {
        assert_eq!(norm("C:/Program Files (x86)/Steam/"), "c:\\program files (x86)\\steam");
        assert_eq!(norm("\\\\?\\D:\\Games\\X"), "d:\\games\\x");
        assert!(game_folder("D:\\SteamLibrary\\steamapps\\common\\Hades", "C:\\Windows"));
        assert!(game_folder("C:\\Games\\Foo", "C:\\Windows"));
        assert!(!game_folder("C:\\Games", "C:\\Windows"), "a top-level folder holds many games");
        assert!(!game_folder("D:\\", "C:\\Windows"));
        assert!(!game_folder("C:\\Windows\\System32", "C:\\Windows"));
        assert!(game_folder("C:\\WindowsApps2\\Foo", "C:\\Windows"));
    }

    #[test]
    fn owners_by_folder_and_executable() {
        let games = vec![
            game(Store::Steam, "1145360", Some("D:\\SteamLibrary\\steamapps\\common\\Hades")),
            game(Store::Epic, "Sugar", Some("C:\\Program Files\\Epic Games\\RocketLeague")),
            game(Store::Steam, "99", None),
            game(Store::Manual, "a", None),
            game(Store::Manual, "b", None),
            game(Store::Manual, "c", None),
        ];
        let mut manual = HashMap::new();
        manual.insert(GameId::new(Store::Manual, "a"), ManualEntry { exe: "C:\\Games\\Celeste\\Celeste.exe".into(), ..Default::default() });
        manual.insert(GameId::new(Store::Manual, "b"), ManualEntry { exe: "C:\\Windows\\System32\\cmd.exe".into(), ..Default::default() });
        manual.insert(GameId::new(Store::Manual, "c"), ManualEntry { exe: "notepad".into(), ..Default::default() });
        let o = owners(&games, &manual, "C:\\Windows");
        assert_eq!(
            o.iter().map(|o| (o.game.0.as_str(), o.path.clone())).collect::<Vec<_>>(),
            vec![
                ("steam:1145360", OwnerPath::Dir("d:\\steamlibrary\\steamapps\\common\\hades".into())),
                ("epic:Sugar", OwnerPath::Dir("c:\\program files\\epic games\\rocketleague".into())),
                ("manual:a", OwnerPath::Dir("c:\\games\\celeste".into())),
                ("manual:b", OwnerPath::Exe("c:\\windows\\system32\\cmd.exe".into())),
            ]
        );
        let tagged = |exe: &str| owner_of(exe, &o, None).map(|g| g.0);
        assert_eq!(tagged("D:\\SteamLibrary\\steamapps\\common\\Hades\\x64\\Hades.exe").as_deref(), Some("steam:1145360"));
        assert_eq!(tagged("d:/steamlibrary/STEAMAPPS/common/hades/Hades.exe").as_deref(), Some("steam:1145360"));
        assert_eq!(tagged("D:\\SteamLibrary\\steamapps\\common\\Hades2\\Hades2.exe"), None, "a sibling folder with a longer name");
        assert_eq!(tagged("C:\\Windows\\System32\\cmd.exe").as_deref(), Some("manual:b"));
        assert_eq!(tagged("C:\\Windows\\System32\\svchost.exe"), None);
        assert_eq!(tagged("C:\\Program Files (x86)\\Steam\\steam.exe"), None);
    }

    #[test]
    fn steam_running_app_picks_between_apps_sharing_a_folder() {
        let dir = "C:\\Steam\\steamapps\\common\\Game";
        let o = owners(&[game(Store::Steam, "200", Some(dir)), game(Store::Steam, "100", Some(dir))], &HashMap::new(), "C:\\Windows");
        let exe = "C:\\Steam\\steamapps\\common\\Game\\game.exe";
        assert_eq!(owner_of(exe, &o, Some(200)), Some(GameId::new(Store::Steam, "200")));
        assert_eq!(owner_of(exe, &o, Some(100)), Some(GameId::new(Store::Steam, "100")));
        // Steam runs neither (or nothing): the same answer every time.
        assert_eq!(owner_of(exe, &o, Some(0)), Some(GameId::new(Store::Steam, "100")));
        assert_eq!(owner_of(exe, &o, None), Some(GameId::new(Store::Steam, "100")));
        // A deeper folder beats RunningAppID.
        let mut o2 = o.clone();
        o2.push(Owner { game: GameId::new(Store::Epic, "Inner"), path: OwnerPath::Dir(norm("C:\\Steam\\steamapps\\common\\Game\\Inner")) });
        assert_eq!(owner_of("C:\\Steam\\steamapps\\common\\Game\\Inner\\x.exe", &o2, Some(200)), Some(GameId::new(Store::Epic, "Inner")));
    }

    #[cfg(windows)]
    #[test]
    fn live_table_sees_this_process() {
        let live = Live::default();
        let me = std::process::id();
        assert!(live.pids().contains(&me));
        let (name, state, _ppid, start) = live.stat(me).unwrap();
        assert!(name.to_ascii_lowercase().ends_with(".exe"), "{name}");
        assert_eq!(state, 'R');
        assert!(start > 0);
        assert_eq!(live::creation_time(me), start);
        let exe = live::exe_path(me).unwrap();
        assert_eq!(exe.file_name(), std::env::current_exe().unwrap().file_name());
        assert_eq!(live.read(me).unwrap().tags, Tags::default());
        let mine = Owner { game: GameId::new(Store::Manual, "t"), path: OwnerPath::Exe(norm(&exe.to_string_lossy())) };
        assert!(live.set_owners(vec![mine.clone()]));
        assert!(!live.set_owners(vec![mine]), "unchanged owners");
        assert_eq!(live.read(me).unwrap().tags.grapple_game.as_deref(), Some("manual:t"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn spawned_children_are_tracked_and_stopped() {
        let fs = crate::proc::ProcFs::default();
        // A console program that waits: `ping` 30 times, one second apart.
        let args: Vec<String> = ["-n", "30", "127.0.0.1"].iter().map(|s| s.to_string()).collect();
        let mut child = crate::spawn::spawn_detached("ping.exe", &args, &Default::default(), None, None, &fs).unwrap();
        assert!(child.starttime > 0, "the start time is known at once");
        let mut scan = crate::proc::Scanner::new(fs.clone());
        scan.refresh();
        let p = scan.get(child.pid).expect("the child is in the table");
        assert_eq!(p.starttime, child.starttime);
        assert!(p.comm.eq_ignore_ascii_case("ping.exe"));
        assert!(!live::terminate_if_same(child.pid, child.starttime + 1), "a different start time is another process");
        assert!(live::terminate_if_same(child.pid, child.starttime));
        let code = tokio::time::timeout(std::time::Duration::from_secs(10), child.exit.wait_for(Option::is_some)).await;
        assert!(code.is_ok(), "the child ended");
    }

    #[test]
    fn tags_and_readiness() {
        assert_eq!(tags_for(Some(GameId::new(Store::Steam, "730"))).steam_app.as_deref(), Some("730"));
        let t = tags_for(Some(GameId::new(Store::Epic, "Sugar")));
        assert_eq!((t.steam_app, t.grapple_game.as_deref()), (None, Some("epic:Sugar")));
        assert_eq!(tags_for(None), Tags::default());
        assert!(steam_ready(true, Some(123)));
        assert!(!steam_ready(true, Some(0)), "the login window is up");
        assert!(!steam_ready(true, None));
        assert!(!steam_ready(false, Some(123)), "a stale pid in the registry");
    }
}
