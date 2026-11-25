//! GameMode for the games Grapple starts itself (Epic, manual).
//!
//! `gamemoderun` works by `LD_PRELOAD=libgamemodeauto.so.0` on everything below it. In front of umu-run that is
//! the whole chain: bwrap, python, pressure-vessel, `objdump`, `pv-verify`, shells. Where the library is not on
//! the default search path (NixOS puts it there through `LD_LIBRARY_PATH`, which the Steam Linux Runtime
//! container replaces) every one of them prints "ld.so: object 'libgamemodeauto.so.0' from LD_PRELOAD cannot be
//! preloaded"; where it does load, each helper registers itself as a game for its few milliseconds.
//!
//! So when `gamemoded` is installed, Grapple asks the daemon for the game's own processes once they run:
//! `gamemoded -r<pid>` (libgamemode's `gamemode_request_start_for`, D-Bus `RegisterGameByPID`). The daemon
//! forgets each one when it exits. Only where `gamemoded` is missing does the `gamemoderun` wrapper stay.

use crate::proc::{Scanner, is_helper, is_protected};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How a launch gets GameMode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    Off,
    /// Register the game's processes with the daemon through this `gamemoded`.
    Register(PathBuf),
    /// Wrap the command in this `gamemoderun` (no `gamemoded` client on `PATH`).
    Wrap(PathBuf),
}

/// What a launch asks for once the game runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub bin: PathBuf,
    /// A Windows program (Wine/Proton): only its `.exe` processes are the game.
    pub windows: bool,
}

/// GameMode for a game that wants it, looked up on `path_var` (the child `PATH`). Off on systems without it.
pub fn choose(want: bool, path_var: &str) -> Via {
    if !want || !cfg!(target_os = "linux") {
        return Via::Off;
    }
    choose_from(true, crate::paths::which_in("gamemoded", path_var), crate::paths::which_in("gamemoderun", path_var))
}

pub fn choose_from(want: bool, gamemoded: Option<PathBuf>, gamemoderun: Option<PathBuf>) -> Via {
    match (want, gamemoded, gamemoderun) {
        (false, _, _) => Via::Off,
        (true, Some(d), _) => Via::Register(d),
        (true, None, Some(r)) => Via::Wrap(r),
        (true, None, None) => Via::Off,
    }
}

/// Is this command line (`argv[0]` of a Wine process) a program of the game, not one of Wine's own
/// (`C:\windows\system32\services.exe` and friends)?
pub fn is_game_exe(argv0: &str) -> bool {
    let lower = argv0.to_ascii_lowercase().replace('/', "\\");
    lower.ends_with(".exe") && !lower.contains(":\\windows\\")
}

/// Which of `candidates` (new members of the game's process tree) to register: for a Windows game the
/// processes Wine runs from the game's own `.exe` files, for a native game every process that is not a
/// leftover helper. `done` holds the pids already asked for.
pub fn targets(scan: &Scanner, candidates: &[u32], members: &HashSet<u32>, windows: bool, done: &HashSet<u32>) -> Vec<u32> {
    let mut out: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|pid| members.contains(pid) && !done.contains(pid))
        .filter(|pid| {
            let Some(p) = scan.get(*pid) else { return false };
            if !p.alive() || is_helper(&p.comm) || is_protected(&p.comm) {
                return false;
            }
            !windows || scan.fs().argv0(*pid).is_some_and(|a| is_game_exe(&a))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Ask the daemon to treat `pid` as a game (blocking, a few milliseconds). `gamemoded -r<pid>` toggles, so a
/// pid GameMode already has (a game that asks for itself) is left alone.
#[cfg(target_os = "linux")]
pub fn register(bin: &Path, pid: u32) -> std::result::Result<(), String> {
    use std::process::{Command, Stdio};
    let run = |flag: &str| {
        Command::new(bin)
            .arg(format!("{flag}{pid}"))
            .env_clear()
            .envs(crate::env::for_child())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {}: {e}", bin.display()))
    };
    let status = run("-s")?;
    let said = String::from_utf8_lossy(&status.stdout);
    if said.contains("registered") && !said.contains("not registered") {
        return Ok(());
    }
    let out = run("-r")?;
    if out.status.success() {
        Ok(())
    } else {
        let mut why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if why.is_empty() {
            why = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(why)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn register(_bin: &Path, _pid: u32) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::ProcFs;

    #[test]
    fn the_daemon_client_wins_over_the_preload_wrapper() {
        let d = PathBuf::from("/usr/bin/gamemoded");
        let r = PathBuf::from("/usr/bin/gamemoderun");
        assert_eq!(choose_from(true, Some(d.clone()), Some(r.clone())), Via::Register(d.clone()));
        assert_eq!(choose_from(true, None, Some(r.clone())), Via::Wrap(r.clone()), "fallback: only the wrapper is installed");
        assert_eq!(choose_from(true, None, None), Via::Off);
        assert_eq!(choose_from(false, Some(d), Some(r)), Via::Off);
        assert_eq!(choose(false, "/usr/bin"), Via::Off);
    }

    #[test]
    fn game_exes_but_not_wines_own_programs() {
        assert!(is_game_exe(r"Z:\home\j\Games\rocketleague\Binaries\Win64\RocketLeague.exe"));
        assert!(is_game_exe(r"C:\Program Files\Epic Games\Launcher.EXE"));
        assert!(!is_game_exe(r"C:\windows\system32\services.exe"));
        assert!(!is_game_exe(r"c:\Windows\SysWOW64\explorer.exe"));
        assert!(!is_game_exe("/nix/store/x-python3/bin/python3.13"));
        assert!(!is_game_exe(""));
    }

    fn fake_proc(dir: &Path, pid: u32, comm: &str, ppid: u32, argv0: &str) {
        let d = dir.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("stat"), format!("{pid} ({comm}) S {ppid} 0 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {pid}0 0 0\n")).unwrap();
        std::fs::write(d.join("environ"), b"").unwrap();
        std::fs::write(d.join("cmdline"), format!("{argv0}\0")).unwrap();
    }

    #[test]
    fn only_the_games_own_processes_are_registered() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        // umu-run -> python -> pressure-vessel -> wine: Rocket League's launcher, the game, and Wine's services
        fake_proc(p, 100, "umu-run", 1, "/home/j/.local/bin/umu-run");
        fake_proc(p, 101, "python3.13", 100, "/nix/store/x/bin/python3.13");
        fake_proc(p, 102, "pv-adverb", 101, "/run/pressure-vessel/pv-adverb");
        fake_proc(p, 103, "Launcher.exe", 102, r"Z:\games\rl\Binaries\Win64\Launcher.exe");
        fake_proc(p, 104, "RocketLeague.ex", 103, r"Z:\games\rl\Binaries\Win64\RocketLeague.exe");
        fake_proc(p, 105, "services.exe", 102, r"C:\windows\system32\services.exe");
        fake_proc(p, 106, "wineserver", 102, "/proton/files/bin/wineserver");
        fake_proc(p, 200, "other.exe", 1, r"Z:\other\other.exe");
        let mut scan = Scanner::new(ProcFs::at(p));
        scan.refresh();
        let members: HashSet<u32> = (100..=106).collect();
        let all: Vec<u32> = (100..=106).chain([200]).collect();

        assert_eq!(targets(&scan, &all, &members, true, &HashSet::new()), [103, 104]);
        // asked once per process
        assert_eq!(targets(&scan, &all, &members, true, &HashSet::from([103])), [104]);
        // a native game: everything of its own that is not a leftover helper
        assert_eq!(targets(&scan, &[100, 101, 106], &members, false, &HashSet::new()), [100, 101]);
    }
}
