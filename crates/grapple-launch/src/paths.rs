//! Where things live. Everything takes `HOME`/`XDG_*` (Windows: `USERPROFILE`/`APPDATA`/`LOCALAPPDATA`) from the
//! environment so tests can point elsewhere; the rules are in [`grapple_core::dirs`].

use std::path::PathBuf;

pub fn home() -> PathBuf {
    grapple_core::dirs::home()
}

pub fn config_home() -> PathBuf {
    grapple_core::dirs::config_home()
}

pub fn data_home() -> PathBuf {
    grapple_core::dirs::data_home()
}

pub fn state_home() -> PathBuf {
    grapple_core::dirs::state_home()
}

/// `$XDG_STATE_HOME/grapple/logs` (`%LOCALAPPDATA%\grapple\logs`): one log file per launched game (stdout and
/// stderr of the game).
pub fn log_dir() -> PathBuf {
    grapple_core::dirs::grapple_state().join("logs")
}

/// Find `name` on `path` (a `PATH`-style string). Returns the first executable file.
#[cfg(not(windows))]
pub fn which_in(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_exec(&p).then_some(p);
    }
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let p = PathBuf::from(dir).join(name);
        if let Ok(m) = std::fs::metadata(&p) {
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Find `name` on `path` (a `PATH`-style string, `;`-separated). Like `cmd.exe`, a name without an extension also
/// matches `name` + each `PATHEXT` extension (`.COM;.EXE;.BAT;.CMD` when unset). Returns the first file found.
#[cfg(windows)]
pub fn which_in(name: &str, path: &str) -> Option<PathBuf> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let find = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && is_exec(&base) {
            return Some(base);
        }
        let file = base.file_name()?.to_string_lossy().into_owned();
        pathext.split(';').filter(|e| !e.is_empty()).map(|e| base.with_file_name(format!("{file}{e}"))).find(|p| is_exec(p))
    };
    if name.contains(['/', '\\']) {
        return find(PathBuf::from(name));
    }
    path.split(';').filter(|d| !d.is_empty()).find_map(|dir| find(PathBuf::from(dir.trim_matches('"')).join(name)))
}

pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var("PATH").unwrap_or_default())
}

#[cfg(not(windows))]
fn is_exec(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Windows has no execute bit: any file is a candidate (CreateProcess decides).
#[cfg(windows)]
fn is_exec(p: &std::path::Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn finds_programs_with_pathext() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.exe"), b"").unwrap();
        let path = format!("C:\\nowhere;{}", dir.path().display());
        assert_eq!(which_in("tool", &path), Some(dir.path().join("tool.exe")));
        assert_eq!(which_in("tool.exe", &path), Some(dir.path().join("tool.exe")));
        assert_eq!(which_in(&dir.path().join("tool.exe").to_string_lossy(), ""), Some(dir.path().join("tool.exe")));
        assert_eq!(which_in("missing", &path), None);
    }
}
