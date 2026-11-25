//! Small helpers for the Settings page that touch the system: which OS this is, opening a folder or file with the
//! desktop's own handler, folder sizes, clearing the cache, the newest game log, and the starter theme.css.
//! Everything that reads or writes disk is called from `spawn_blocking` by the commands. The mock backend answers
//! with fixtures, so most of this is only used by the real one.
#![cfg_attr(not(feature = "real"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// `linux`, `windows` or `macos`: the UI hides the rows a system does not have.
pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Open a folder or file with Explorer (through the shell, see [`shell_open`]).
#[cfg(windows)]
pub fn open(path: &Path) -> std::io::Result<()> {
    shell_open(&path.to_string_lossy())
}

/// Open a folder or file with the desktop's handler (`xdg-open`, `open`). On Linux it gets the session
/// environment grapple started with (never grapple's own variables), like the tether sign-in browser.
#[cfg(not(windows))]
pub fn open(path: &Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(all(feature = "real", target_os = "linux"))]
    cmd.env_clear().envs(grapple_launch::env::for_child());
    let mut child = cmd.spawn()?;
    // Reap it without blocking the caller.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Open a file, folder or URL with its Windows handler (`ShellExecuteW` "open": Explorer for folders, the default
/// browser for web links). Nothing is parsed by a shell, so commas, spaces and `&` in it are safe.
#[cfg(windows)]
pub fn shell_open(target: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain([0]).collect() };
    let (verb, file) = (wide("open"), wide(target));
    // SAFETY: NUL-terminated strings; no window owner, parameters or directory.
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    // ShellExecuteW returns a value greater than 32 on success.
    if r as usize > 32 { Ok(()) } else { Err(std::io::Error::other(format!("ShellExecute failed ({})", r as usize))) }
}

/// Total size of the files under `dir` (0 when it does not exist). Links are not followed.
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Delete everything inside `dir` (the folder itself stays). Returns the bytes freed. A missing folder is fine.
pub fn clear_dir(dir: &Path) -> std::io::Result<u64> {
    let before = dir_size(dir);
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let r = if is_dir { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        if let Err(err) = r {
            tracing::warn!("clear cache: {}: {err}", p.display());
        }
    }
    Ok(before.saturating_sub(dir_size(dir)))
}

/// The most recently written file in `dir` (the game logs: one file per launch).
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// The end of a text file, at most `max` bytes (cut at a line start), lossily decoded.
pub fn tail(path: &Path, max: usize) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let start = bytes.len().saturating_sub(max);
    let mut slice = &bytes[start..];
    if start > 0
        && let Some(nl) = slice.iter().position(|b| *b == b'\n')
    {
        slice = &slice[nl + 1..];
    }
    Ok(String::from_utf8_lossy(slice).into_owned())
}

/// A game log as it may be copied: the Epic exchange code (`-AUTH_PASSWORD=…`) and anything named like a token is
/// replaced, in case a game or Proton echoed its command line.
pub fn redact_secrets(text: &str) -> String {
    const KEYS: [&str; 5] = ["AUTH_PASSWORD=", "access_token", "refresh_token", "exchange_code", "authorization:"];
    text.lines()
        .map(|line| {
            let mut out = line.to_string();
            for key in KEYS {
                let lower = out.to_ascii_lowercase();
                let Some(at) = lower.find(&key.to_ascii_lowercase()) else { continue };
                let start = at + key.len();
                // Skip `": "`, `=`, spaces before the value; the value ends at whitespace, a quote or a comma.
                let skip = out[start..].find(|c: char| !matches!(c, '"' | '\'' | ':' | '=' | ' ')).map_or(out.len(), |i| start + i);
                let end = out[skip..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',')).map_or(out.len(), |i| skip + i);
                if end > skip {
                    out.replace_range(skip..end, "<hidden>");
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if text.ends_with('\n') { "\n" } else { "" }
}

/// What a new theme.css starts with: every line a comment, so creating it changes nothing.
pub const THEME_STARTER: &str = "/* grapple theme override.
 * Any token from grapple's tokens.css can be set here; this file is read after it, so it wins.
 * Save, then switch back to grapple: the change shows at once.
 *
 * :root {
 *   --accent: #4ade80;
 *   --canvas: #0c0c0e;
 *   --panel: #18181b;
 *   --radius-card: 14px;
 * }
 */
";

/// Create `path` with the starter (and its folder) unless it exists. `true` when it was created.
pub fn ensure_theme_file(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, THEME_STARTER)?;
    Ok(true)
}

/// `~/…` for paths under the home folder (what the UI shows).
pub fn tilde(p: &Path) -> String {
    let home = grapple_core::dirs::home();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() && home != Path::new("/") => {
            if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.to_string_lossy()) }
        }
        _ => p.to_string_lossy().into_owned(),
    }
}

/// `$XDG_DATA_HOME/grapple`: the database, picked art, the store window's data (`%LOCALAPPDATA%\grapple`).
pub fn data_dir() -> PathBuf {
    grapple_core::dirs::grapple_data()
}

/// `$XDG_CACHE_HOME/grapple`: profile pictures, tether game icons, store pictures (`%LOCALAPPDATA%\grapple\cache`).
pub fn cache_dir() -> PathBuf {
    grapple_core::dirs::grapple_cache()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grapple-system-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cache_size_and_clear() {
        let d = tmp("cache");
        std::fs::create_dir_all(d.join("avatars")).unwrap();
        std::fs::create_dir_all(d.join("store-art/ab")).unwrap();
        std::fs::write(d.join("avatars/1.png"), [0u8; 100]).unwrap();
        std::fs::write(d.join("store-art/ab/c.jpg"), [0u8; 250]).unwrap();
        std::fs::write(d.join("loose"), [0u8; 5]).unwrap();
        assert_eq!(dir_size(&d), 355);
        assert_eq!(clear_dir(&d).unwrap(), 355);
        assert!(d.is_dir(), "the folder itself stays");
        assert_eq!(dir_size(&d), 0);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        // A cache that was never made: nothing to clear.
        assert_eq!(clear_dir(&d.join("missing")).unwrap(), 0);
        assert_eq!(dir_size(&d.join("missing")), 0);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn newest_log_and_its_tail() {
        let d = tmp("logs");
        assert!(newest_file(&d).is_none());
        std::fs::write(d.join("old.log"), "old\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.join("new.log"), "line one\nline two\nline three\n").unwrap();
        assert_eq!(newest_file(&d), Some(d.join("new.log")));
        assert_eq!(tail(&d.join("new.log"), 1000).unwrap(), "line one\nline two\nline three\n");
        // Cut to whole lines.
        assert_eq!(tail(&d.join("new.log"), 14).unwrap(), "line three\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn copied_logs_hide_secrets() {
        let log = "wine: run Game.exe -AUTH_LOGIN=unused -AUTH_PASSWORD=abc123def -AUTH_TYPE=exchangecode\n{\"access_token\": \"eg1~xyz\", \"n\": 1}\nplain line\n";
        let out = redact_secrets(log);
        assert!(!out.contains("abc123def") && !out.contains("eg1~xyz"), "{out}");
        assert!(out.contains("-AUTH_PASSWORD=<hidden> -AUTH_TYPE=exchangecode"), "{out}");
        assert!(out.contains("\"access_token\": \"<hidden>\""), "{out}");
        assert!(out.ends_with("plain line\n"));
    }

    #[test]
    fn theme_starter_is_created_once() {
        let d = tmp("theme");
        let p = d.join("grapple").join("theme.css");
        assert!(ensure_theme_file(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), THEME_STARTER);
        std::fs::write(&p, ":root { --accent: red; }").unwrap();
        assert!(!ensure_theme_file(&p).unwrap(), "an existing file is never replaced");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), ":root { --accent: red; }");
        // Every line of the starter is inside the one comment: it changes nothing.
        assert!(THEME_STARTER.trim_start().starts_with("/*") && THEME_STARTER.trim_end().ends_with("*/"));
        assert_eq!(THEME_STARTER.matches("*/").count(), 1);
        std::fs::remove_dir_all(&d).ok();
    }
}
