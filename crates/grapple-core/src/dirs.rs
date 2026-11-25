//! Where things live, per platform. Every grapple crate asks here instead of reading `HOME`/`XDG_*` itself.
//!
//! * Linux (and any other Unix): the XDG base directories (`$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, ... with the
//!   usual fallbacks under `$HOME`). A relative or empty value is ignored, as the spec says.
//! * Windows: the user's known folders from the environment Windows sets for every session: config in `%APPDATA%`
//!   (roaming), data, state and cache in `%LOCALAPPDATA%` (this machine only; a database does not belong in a
//!   roaming profile), home in `%USERPROFILE%`.
//!
//! The base folders ([`config_home`], [`data_home`], ...) are also where other programs keep their files (Heroic's
//! `heroic` folder sits in `%APPDATA%` on Windows and in `~/.config` on Linux), so they stay public.

use std::ffi::OsString;
use std::path::PathBuf;

/// Which base folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Config,
    Data,
    State,
    Cache,
}

/// Which convention (a parameter so both can be tested on any machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Unix,
    Windows,
}

impl Os {
    pub const fn current() -> Os {
        if cfg!(windows) { Os::Windows } else { Os::Unix }
    }
}

/// Is `p` a full path by `os`'s rules (`/...`; `C:\...` or `\\server\...`)? Plain text, so both conventions can be
/// checked on any machine.
fn is_absolute(os: Os, p: &OsString) -> bool {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    match os {
        Os::Unix => s.starts_with('/'),
        Os::Windows => (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')) || s.starts_with("\\\\"),
    }
}

/// The base folder `base` for `os`, reading variables through `var`.
pub fn resolve(os: Os, base: Base, var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let absolute = |name: &str| var(name).filter(|p| is_absolute(os, p)).map(PathBuf::from);
    let home = home_with(os, &var);
    match os {
        Os::Unix => {
            let (name, fallback) = match base {
                Base::Config => ("XDG_CONFIG_HOME", ".config"),
                Base::Data => ("XDG_DATA_HOME", ".local/share"),
                Base::State => ("XDG_STATE_HOME", ".local/state"),
                Base::Cache => ("XDG_CACHE_HOME", ".cache"),
            };
            absolute(name).unwrap_or_else(|| home.join(fallback))
        }
        Os::Windows => match base {
            Base::Config => absolute("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")),
            Base::Data | Base::State | Base::Cache => absolute("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local")),
        },
    }
}

fn home_with(os: Os, var: &impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let names: &[&str] = match os {
        Os::Unix => &["HOME"],
        Os::Windows => &["USERPROFILE", "HOME"],
    };
    names
        .iter()
        .find_map(|n| var(n).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if os == Os::Windows { "C:\\" } else { "/" }))
}

fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The user's home folder (`$HOME`; `%USERPROFILE%` on Windows).
pub fn home() -> PathBuf {
    home_with(Os::current(), &env)
}

/// `$XDG_CONFIG_HOME` (`~/.config`); `%APPDATA%` on Windows.
pub fn config_home() -> PathBuf {
    resolve(Os::current(), Base::Config, env)
}

/// `$XDG_DATA_HOME` (`~/.local/share`); `%LOCALAPPDATA%` on Windows.
pub fn data_home() -> PathBuf {
    resolve(Os::current(), Base::Data, env)
}

/// `$XDG_STATE_HOME` (`~/.local/state`); `%LOCALAPPDATA%` on Windows.
pub fn state_home() -> PathBuf {
    resolve(Os::current(), Base::State, env)
}

/// `$XDG_CACHE_HOME` (`~/.cache`); `%LOCALAPPDATA%` on Windows.
pub fn cache_home() -> PathBuf {
    resolve(Os::current(), Base::Cache, env)
}

/// grapple's own folder of kind `base`, for `os`. On Windows data, state and cache share `%LOCALAPPDATA%`, so the
/// cache gets its own subfolder there.
pub fn grapple_with(os: Os, base: Base, var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let dir = resolve(os, base, var).join("grapple");
    match (os, base) {
        (Os::Windows, Base::Cache) => dir.join("cache"),
        _ => dir,
    }
}

/// `$XDG_CONFIG_HOME/grapple`; `%APPDATA%\grapple` on Windows.
pub fn grapple_config() -> PathBuf {
    grapple_with(Os::current(), Base::Config, env)
}

/// `$XDG_DATA_HOME/grapple`: the database, picked art, the store window's data; `%LOCALAPPDATA%\grapple` on Windows.
pub fn grapple_data() -> PathBuf {
    grapple_with(Os::current(), Base::Data, env)
}

/// `$XDG_STATE_HOME/grapple` (game logs); `%LOCALAPPDATA%\grapple` on Windows.
pub fn grapple_state() -> PathBuf {
    grapple_with(Os::current(), Base::State, env)
}

/// `$XDG_CACHE_HOME/grapple`; `%LOCALAPPDATA%\grapple\cache` on Windows.
pub fn grapple_cache() -> PathBuf {
    grapple_with(Os::current(), Base::Cache, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let m: HashMap<String, OsString> = pairs.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn unix_follows_xdg_with_home_fallbacks() {
        let v = vars(&[("HOME", "/home/j"), ("XDG_CONFIG_HOME", "/cfg"), ("XDG_DATA_HOME", "relative/ignored")]);
        assert_eq!(resolve(Os::Unix, Base::Config, &v), PathBuf::from("/cfg"));
        assert_eq!(resolve(Os::Unix, Base::Data, &v), PathBuf::from("/home/j/.local/share"));
        assert_eq!(resolve(Os::Unix, Base::State, &v), PathBuf::from("/home/j/.local/state"));
        assert_eq!(resolve(Os::Unix, Base::Cache, &v), PathBuf::from("/home/j/.cache"));
        assert_eq!(grapple_with(Os::Unix, Base::Cache, &v), PathBuf::from("/home/j/.cache/grapple"));
        assert_eq!(home_with(Os::Unix, &vars(&[])), PathBuf::from("/"));
    }

    #[test]
    fn windows_uses_known_folders() {
        let (roaming, local) = ("C:\\Users\\j\\AppData\\Roaming", "C:\\Users\\j\\AppData\\Local");
        let v = vars(&[("USERPROFILE", "C:\\Users\\j"), ("APPDATA", roaming), ("LOCALAPPDATA", local), ("XDG_CONFIG_HOME", "/nope")]);
        assert_eq!(resolve(Os::Windows, Base::Config, &v), PathBuf::from(roaming));
        for base in [Base::Data, Base::State, Base::Cache] {
            assert_eq!(resolve(Os::Windows, base, &v), PathBuf::from(local));
        }
        assert_eq!(grapple_with(Os::Windows, Base::Data, &v), PathBuf::from(local).join("grapple"));
        assert_eq!(grapple_with(Os::Windows, Base::Cache, &v), PathBuf::from(local).join("grapple").join("cache"));
        assert_eq!(grapple_with(Os::Windows, Base::Config, &v), PathBuf::from(roaming).join("grapple"));
        // A relative value is ignored, as on Linux.
        let rel = vars(&[("USERPROFILE", "/Users/j"), ("APPDATA", "AppData")]);
        assert_eq!(resolve(Os::Windows, Base::Config, &rel), PathBuf::from("/Users/j/AppData/Roaming"));
        assert!(is_absolute(Os::Windows, &"\\\\server\\share".into()) && is_absolute(Os::Windows, &"d:/x".into()));
        // Missing variables: the standard layout under the profile.
        let bare = vars(&[("USERPROFILE", "/Users/j")]);
        assert_eq!(resolve(Os::Windows, Base::Config, &bare), PathBuf::from("/Users/j/AppData/Roaming"));
        assert_eq!(resolve(Os::Windows, Base::Data, &bare), PathBuf::from("/Users/j/AppData/Local"));
        assert_eq!(home_with(Os::Windows, &vars(&[("HOME", "/h")])), PathBuf::from("/h"));
    }
}
