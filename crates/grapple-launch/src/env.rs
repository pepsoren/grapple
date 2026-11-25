//! The environment of everything Grapple starts (Steam, Proton/umu, manual games, `steam://` URLs).
//!
//! Children get Josh's session environment as it was when Grapple started, never what Grapple added for its own
//! window (`WEBKIT_*`, `__NV_*` workarounds, whatever GTK/WebKit/Tauri set later) and never Grapple's own
//! switches (`GRAPPLE_*`). The `GRAPPLE_GAME` marker the tracker needs is added per launch by the plan, after
//! this base.
//!
//! The app calls [`capture`] as the first statement of `main` and sets its own variables through [`set_own`].
//! Without a capture (tests, other tools) the current environment is used, minus the [`set_own`] keys.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, OnceLock};

static PRISTINE: OnceLock<Vec<(OsString, OsString)>> = OnceLock::new();
static OWN: Mutex<Vec<OsString>> = Mutex::new(Vec::new());

/// Remember the environment as it is now. Call first thing in `main`, before anything sets a variable.
/// Later calls do nothing.
pub fn capture() {
    PRISTINE.get_or_init(|| std::env::vars_os().collect());
}

/// Set a variable for Grapple's own process (its window, WebKit's helper processes). Children never see it,
/// unless it was already in the captured session environment, in which case they get the session's value.
///
/// # Safety
/// Same contract as [`std::env::set_var`]: no other thread may read or write the environment at the same time
/// (call it at the start of `main`).
pub unsafe fn set_own(key: &str, value: &str) {
    capture();
    OWN.lock().unwrap_or_else(|e| e.into_inner()).push(OsString::from(key));
    // SAFETY: forwarded to the caller (see above).
    unsafe { std::env::set_var(key, value) };
}

/// Variables a child never inherits from Grapple, even when they were in the session environment.
fn never_inherited(key: &OsStr) -> bool {
    let k = key.as_encoded_bytes();
    k.starts_with(b"GRAPPLE_")
        || k.starts_with(b"WEBKIT_")
        // Grapple's own launch context: a stale activation token would let Steam's window steal focus,
        // and the desktop-file markers would name Grapple.
        || matches!(k, b"DESKTOP_STARTUP_ID" | b"XDG_ACTIVATION_TOKEN" | b"GIO_LAUNCHED_DESKTOP_FILE" | b"GIO_LAUNCHED_DESKTOP_FILE_PID")
        // The AppImage runtime's own markers (Grapple packaged as an AppImage).
        || matches!(k, b"APPDIR" | b"APPIMAGE" | b"ARGV0" | b"OWD")
}

/// Remove what an AppImage of Grapple put into the environment: path-list entries inside `$APPDIR`, and
/// variables that point into it.
fn without_appdir(env: Vec<(OsString, OsString)>) -> Vec<(OsString, OsString)> {
    let Some(appdir) = env.iter().find(|(k, _)| k == "APPDIR").map(|(_, v)| v.as_encoded_bytes().to_vec()) else {
        return env;
    };
    if appdir.len() < 2 {
        return env;
    }
    let inside = |s: &[u8]| s.starts_with(&appdir);
    env.into_iter()
        .filter_map(|(k, v)| {
            let bytes = v.as_encoded_bytes();
            if !bytes.contains(&b':') {
                return (!inside(bytes)).then_some((k, v));
            }
            let kept: Vec<&[u8]> = bytes.split(|b| *b == b':').filter(|p| !p.is_empty() && !inside(p)).collect();
            if kept.is_empty() {
                return None;
            }
            let joined = kept.join(&b':');
            // SAFETY: the bytes are pieces of an OsString's encoded bytes joined by ASCII ':', which keeps them
            // valid encoded bytes.
            Some((k, unsafe { OsString::from_encoded_bytes_unchecked(joined) }))
        })
        .collect()
}

/// The environment for a child process: the session environment without Grapple's additions.
pub fn for_child() -> Vec<(OsString, OsString)> {
    let base: Vec<(OsString, OsString)> = match PRISTINE.get() {
        Some(p) => p.clone(),
        None => {
            let own = OWN.lock().unwrap_or_else(|e| e.into_inner()).clone();
            std::env::vars_os().filter(|(k, _)| !own.contains(k)).collect()
        }
    };
    without_appdir(base.into_iter().filter(|(k, _)| !never_inherited(k)).collect())
}

/// Look up one variable of the child environment (e.g. its `PATH`).
pub fn child_var(key: &str) -> Option<OsString> {
    for_child().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `PATH` of the child environment (where programs Grapple starts are looked up).
pub fn child_path() -> String {
    child_var("PATH").map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(k: &str, v: &str) -> (OsString, OsString) {
        (k.into(), v.into())
    }

    #[test]
    fn drops_grapple_and_webkit_and_launch_context() {
        for k in ["GRAPPLE_PERF", "WEBKIT_DISABLE_DMABUF_RENDERER", "DESKTOP_STARTUP_ID", "XDG_ACTIVATION_TOKEN", "APPIMAGE"] {
            assert!(never_inherited(OsStr::new(k)), "{k}");
        }
        for k in ["HOME", "PATH", "__GLX_VENDOR_LIBRARY_NAME", "GDK_BACKEND", "SteamAppId"] {
            assert!(!never_inherited(OsStr::new(k)), "{k}");
        }
    }

    #[test]
    fn appimage_paths_are_scrubbed() {
        let env = vec![
            kv("APPDIR", "/tmp/.mount_grapple"),
            kv("PATH", "/tmp/.mount_grapple/usr/bin:/usr/bin:/bin"),
            kv("LD_LIBRARY_PATH", "/tmp/.mount_grapple/usr/lib"),
            kv("GDK_PIXBUF_MODULE_FILE", "/tmp/.mount_grapple/usr/lib/loaders.cache"),
            kv("HOME", "/home/j"),
        ];
        let out = without_appdir(env);
        assert!(out.contains(&kv("PATH", "/usr/bin:/bin")));
        assert!(out.contains(&kv("HOME", "/home/j")));
        assert!(!out.iter().any(|(k, _)| k == "LD_LIBRARY_PATH" || k == "GDK_PIXBUF_MODULE_FILE"));
    }
}
