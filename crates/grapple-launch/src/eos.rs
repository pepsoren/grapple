//! Epic's in-game overlay ("Epic Online Services Overlay") in a game's Wine prefix. Games such as Rocket League
//! show their store checkout in it; without it "buy" does nothing. On Windows the Epic launcher sets it up; under
//! Wine/Proton the game finds it through one registry value, `OverlayPath` under `HKCU\Software\Epic Games\EOS`,
//! which lives in the prefix's `user.reg` (a text file). grapple writes or removes that value just before it starts
//! the game, never while Wine runs in the prefix (wineserver writes user.reg back when it exits).

use std::io;
use std::path::{Path, PathBuf};

/// The file that marks a folder as an overlay install.
pub const MARKER: &str = "EOSOVH-Win64-Shipping.dll";

const KEY: &str = r"software\\epic games\\eos";
const HEADER: &str = r"[Software\\Epic Games\\EOS]";
const VALUE: &str = "\"overlaypath\"=";

/// What [`sync`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    Unchanged,
    Enabled,
    Disabled,
    /// The prefix has no user.reg yet (its first run creates it): the overlay is set up on the next start.
    NoPrefixYet,
    /// Wine runs in this prefix now, so user.reg was left alone.
    WineRunning,
}

/// Make the prefix point at `want` (an overlay folder), or, with `None`, remove an `OverlayPath` that points at
/// `ours` (grapple's own overlay; one set up by Heroic or by hand is left alone).
pub fn sync(prefix: &Path, want: Option<&Path>, ours: Option<&Path>) -> io::Result<Synced> {
    let reg = prefix.join("user.reg");
    let text = match std::fs::read_to_string(&reg) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Synced::NoPrefixYet),
        Err(e) => return Err(e),
    };
    let (next, what) = match want {
        Some(dir) => match set_value(&text, &win_path(dir)) {
            Some(t) => (t, Synced::Enabled),
            None => return Ok(Synced::Unchanged),
        },
        None => match ours.and_then(|o| remove_value(&text, o)) {
            Some(t) => (t, Synced::Disabled),
            None => return Ok(Synced::Unchanged),
        },
    };
    if wine_running(prefix) {
        return Ok(Synced::WineRunning);
    }
    let tmp = prefix.join(".user.reg.grapple");
    std::fs::write(&tmp, next)?;
    if let Ok(meta) = std::fs::metadata(&reg) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &reg)?;
    Ok(what)
}

/// `/home/x/overlay` → `Z:/home/x/overlay` (forward slashes work for the overlay and need no escaping).
fn win_path(dir: &Path) -> String {
    let s = dir.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') { format!("Z:{s}") } else { s }
}

/// A Wine .reg string: backslashes and quotes escaped.
fn reg_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn is_header(line: &str) -> bool {
    line.starts_with('[')
}

fn is_our_header(line: &str) -> bool {
    line.split_once(']').is_some_and(|(k, _)| k[1..].eq_ignore_ascii_case(KEY))
}

fn is_value(line: &str) -> bool {
    line.len() >= VALUE.len() && line[..VALUE.len()].eq_ignore_ascii_case(VALUE)
}

/// Where the EOS section's lines are: (header index, index after its last line).
fn section(lines: &[&str]) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| is_our_header(l))?;
    let end = lines[start + 1..].iter().position(|l| is_header(l)).map_or(lines.len(), |i| start + 1 + i);
    Some((start, end))
}

/// The text with `OverlayPath` set to `path`; `None` when it already is.
fn set_value(text: &str, path: &str) -> Option<String> {
    let line = format!("\"OverlayPath\"={}", reg_string(path));
    let mut lines: Vec<&str> = text.lines().collect();
    match section(&lines) {
        Some((start, end)) => match (start + 1..end).find(|&i| is_value(lines[i])) {
            Some(i) if lines[i] == line => return None,
            Some(i) => lines[i] = &line,
            None => {
                // After the header and its `#time=` line, if any.
                let at = if lines.get(start + 1).is_some_and(|l| l.starts_with('#')) { start + 2 } else { start + 1 };
                lines.insert(at, &line);
            }
        },
        None => {
            while lines.last().is_some_and(|l| l.is_empty()) {
                lines.pop();
            }
            lines.push("");
            lines.push(HEADER);
            lines.push(&line);
        }
    }
    Some(lines.join("\n") + "\n")
}

/// The text without an `OverlayPath` that points at `ours`; `None` when there is none.
fn remove_value(text: &str, ours: &Path) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (start, end) = section(&lines)?;
    let i = (start + 1..end).find(|&i| is_value(lines[i]))?;
    let value = lines[i][VALUE.len()..].trim().trim_matches('"').replace("\\\\", "/").replace('\\', "/");
    let ours = win_path(ours);
    if !value.trim_end_matches('/').eq_ignore_ascii_case(ours.trim_end_matches('/')) {
        return None;
    }
    let mut out: Vec<&str> = lines;
    out.remove(i);
    Some(out.join("\n") + "\n")
}

/// Whether a wineserver runs for this prefix (its `WINEPREFIX`, or Proton's `STEAM_COMPAT_DATA_PATH`).
#[cfg(target_os = "linux")]
fn wine_running(prefix: &Path) -> bool {
    let want = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(comm) = std::fs::read_to_string(p.join("comm")) else { continue };
        if comm.trim() != "wineserver" {
            continue;
        }
        let Ok(env) = std::fs::read(p.join("environ")) else { continue };
        for kv in env.split(|b| *b == 0) {
            let kv = String::from_utf8_lossy(kv);
            for key in ["WINEPREFIX=", "STEAM_COMPAT_DATA_PATH="] {
                if let Some(v) = kv.strip_prefix(key) {
                    let v = PathBuf::from(v);
                    let v = std::fs::canonicalize(&v).unwrap_or(v);
                    if v == want || v.join("pfx") == want || want.join("pfx") == v {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn wine_running(_prefix: &Path) -> bool {
    false
}

/// What grapple writes to `HKCU\Software\Epic Games\EOS\OverlayPath` on Windows, if anything: its own overlay
/// (`want`) when the value is missing or points at a folder without the overlay (`current_ok` false). The Epic Games
/// Launcher installs the overlay for every game itself and sets that value (so does legendary's `eos-overlay
/// enable`); a working one is never replaced. The value is per user, not per game, so turning a game's switch off
/// changes nothing on Windows.
pub fn windows_value(current: Option<&str>, current_ok: bool, want: Option<&Path>) -> Option<PathBuf> {
    let want = want?;
    match current {
        Some(c) if !c.trim().is_empty() && current_ok => None,
        _ => Some(want.to_path_buf()),
    }
}

/// The Windows registry key and value of the overlay's location.
pub const WINDOWS_KEY: &str = r"Software\Epic Games\EOS";
pub const WINDOWS_VALUE: &str = "OverlayPath";

/// [`sync`] for Windows, where the game reads the real registry (see [`windows_value`]).
// untested on Windows: the value name and key match legendary's `eos-overlay enable` (utils/eos.py); legendary also
// registers the overlay's Vulkan layers, which grapple does not (as on Linux), so Vulkan games may not show it.
#[cfg(windows)]
pub fn sync_windows(want: Option<&Path>) -> io::Result<Synced> {
    use grapple_core::winreg::{Hive, read_string, write_string};
    let current = read_string(Hive::CurrentUser, WINDOWS_KEY, WINDOWS_VALUE);
    let ok = current.as_deref().is_some_and(|c| Path::new(c.trim()).join(MARKER).is_file());
    match windows_value(current.as_deref(), ok, want) {
        Some(dir) => {
            write_string(WINDOWS_KEY, WINDOWS_VALUE, &dir.to_string_lossy())?;
            Ok(Synced::Enabled)
        }
        None => Ok(Synced::Unchanged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_keeps_a_working_overlay() {
        let ours = Path::new("C:/Users/j/AppData/Local/grapple/eos-overlay");
        assert_eq!(windows_value(None, false, Some(ours)), Some(ours.to_path_buf()));
        assert_eq!(windows_value(Some(""), false, Some(ours)), Some(ours.to_path_buf()));
        assert_eq!(windows_value(Some("C:/gone"), false, Some(ours)), Some(ours.to_path_buf()), "a stale value is replaced");
        assert_eq!(windows_value(Some("C:/Program Files (x86)/Epic Games/Launcher/Portal/Extras/Overlay"), true, Some(ours)), None);
        assert_eq!(windows_value(None, false, None), None, "nothing to offer: leave it");
    }

    const REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\n#arch=win64\n\n[Software\\\\Wine\\\\DllOverrides] 1761000000\n#time=1dc4000000000000\n\"d3d11\"=\"native\"\n";

    fn tmp_prefix(name: &str, reg: Option<&str>) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grapple-eos-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        if let Some(r) = reg {
            std::fs::write(d.join("user.reg"), r).unwrap();
        }
        d
    }

    #[test]
    fn enable_adds_the_section_then_is_idempotent() {
        let p = tmp_prefix("add", Some(REG));
        let ov = Path::new("/home/j/.local/share/grapple/eos-overlay");
        assert_eq!(sync(&p, Some(ov), Some(ov)).unwrap(), Synced::Enabled);
        let t = std::fs::read_to_string(p.join("user.reg")).unwrap();
        assert!(t.starts_with("WINE REGISTRY Version 2\n"), "the rest of the file is kept");
        assert!(t.contains("\"d3d11\"=\"native\""));
        assert!(t.contains("[Software\\\\Epic Games\\\\EOS]\n\"OverlayPath\"=\"Z:/home/j/.local/share/grapple/eos-overlay\"\n"));
        assert_eq!(sync(&p, Some(ov), Some(ov)).unwrap(), Synced::Unchanged);
        assert_eq!(sync(&p, None, Some(ov)).unwrap(), Synced::Disabled);
        let t = std::fs::read_to_string(p.join("user.reg")).unwrap();
        assert!(!t.contains("OverlayPath"));
        assert_eq!(sync(&p, None, Some(ov)).unwrap(), Synced::Unchanged);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn existing_section_and_value_are_updated_in_place() {
        let reg = format!("{REG}\n[Software\\\\Epic Games\\\\EOS] 1761000001\n#time=1dc4\n\"OverlayPath\"=\"Z:/old\"\n\"Other\"=\"x\"\n\n[Software\\\\Zed] 1\n");
        let p = tmp_prefix("upd", Some(&reg));
        let ov = Path::new("/new/overlay");
        assert_eq!(sync(&p, Some(ov), None).unwrap(), Synced::Enabled);
        let t = std::fs::read_to_string(p.join("user.reg")).unwrap();
        assert!(t.contains("#time=1dc4\n\"OverlayPath\"=\"Z:/new/overlay\"\n\"Other\"=\"x\""));
        assert_eq!(t.matches("OverlayPath").count(), 1);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn someone_elses_overlay_is_left_alone() {
        let reg = format!("{REG}\n[Software\\\\Epic Games\\\\EOS]\n\"OverlayPath\"=\"Z:/home/j/Games/.overlay\"\n");
        let p = tmp_prefix("other", Some(&reg));
        assert_eq!(sync(&p, None, Some(Path::new("/home/j/.local/share/grapple/eos-overlay"))).unwrap(), Synced::Unchanged);
        assert!(std::fs::read_to_string(p.join("user.reg")).unwrap().contains("Games/.overlay"));
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn a_new_prefix_waits_for_its_first_run() {
        let p = tmp_prefix("new", None);
        assert_eq!(sync(&p, Some(Path::new("/o")), None).unwrap(), Synced::NoPrefixYet);
        std::fs::remove_dir_all(&p).ok();
    }
}
