//! Opt-in start at login: an XDG autostart entry (`$XDG_CONFIG_HOME/autostart/grapple.desktop`); on Windows a
//! `grapple` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! It starts Grapple hidden (`--hidden`, "Start hidden in the tray") or with its window; the tray icon or running
//! `grapple` again brings a hidden one up.

#[cfg(not(windows))]
use crate::backend::config_dir;
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

/// The Run key of the current user and grapple's value in it.
#[cfg_attr(not(windows), allow(dead_code))]
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg_attr(not(windows), allow(dead_code))]
const RUN_VALUE: &str = "grapple";

/// The Run value for `exe`: the program quoted (paths have spaces), then `--hidden` when wanted.
#[cfg_attr(not(windows), allow(dead_code))]
fn run_command(exe: &Path, hidden: bool) -> String {
    let flag = if hidden { " --hidden" } else { "" };
    format!("\"{}\"{flag}", exe.display())
}

/// Does a Run value start grapple hidden? (No value: the setting's default, hidden.)
#[cfg_attr(not(windows), allow(dead_code))]
fn run_command_hidden(cmd: Option<&str>) -> bool {
    match cmd {
        // The program is quoted, so only the arguments after the closing quote count.
        Some(c) => c.rsplit_once('"').map(|(_, args)| args).unwrap_or(c).split_whitespace().any(|w| w == "--hidden"),
        None => true,
    }
}

#[cfg(windows)]
pub fn set(enabled: bool, hidden: bool) -> std::io::Result<()> {
    use grapple_core::winreg::{delete_value, write_string};
    if !enabled {
        return delete_value(RUN_KEY, RUN_VALUE);
    }
    write_string(RUN_KEY, RUN_VALUE, &run_command(&std::env::current_exe()?, hidden))
}

#[cfg(windows)]
fn run_value() -> Option<String> {
    grapple_core::winreg::read_string(grapple_core::winreg::Hive::CurrentUser, RUN_KEY, RUN_VALUE)
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    run_value().is_some()
}

#[cfg(windows)]
pub fn starts_hidden() -> bool {
    run_command_hidden(run_value().as_deref())
}

#[cfg(not(windows))]
fn entry_path() -> PathBuf {
    config_dir().join("autostart").join("grapple.desktop")
}

/// The desktop entry for `exe`.
#[cfg(not(windows))]
fn entry_body(exe: &Path, hidden: bool) -> String {
    let exec = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let flag = if hidden { " --hidden" } else { "" };
    format!(
        "[Desktop Entry]\nType=Application\nName=grapple\nComment=Game launcher\nExec=\"{exec}\"{flag}\nIcon=grapple\nTerminal=false\nCategories=Game;\nX-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(not(windows))]
pub fn set(enabled: bool, hidden: bool) -> std::io::Result<()> {
    write_entry(&entry_path(), enabled, hidden, program)
}

#[cfg(not(windows))]
fn write_entry(path: &Path, enabled: bool, hidden: bool, exe: impl FnOnce() -> std::io::Result<PathBuf>) -> std::io::Result<()> {
    if !enabled {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, entry_body(&exe()?, hidden))
}

/// The program to start at login: the installed one (`~/.local/bin/grapple`, from `scripts/install-linux.sh`) when
/// it exists, else this process's own file. Linux names a replaced running file `<path> (deleted)`; that suffix
/// is dropped so a rebuild while grapple runs never leaves a broken entry.
#[cfg(not(windows))]
fn program() -> std::io::Result<PathBuf> {
    let installed = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin/grapple"));
    if let Some(p) = installed.filter(|p| p.is_file()) {
        return Ok(p);
    }
    let exe = std::env::current_exe()?;
    Ok(match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => exe,
    })
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    entry_path().exists()
}

/// Does the login entry start grapple hidden? (The file is the truth; no entry: the setting's default, hidden.)
#[cfg(not(windows))]
pub fn starts_hidden() -> bool {
    entry_hidden(&entry_path())
}

#[cfg(not(windows))]
fn entry_hidden(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().any(|l| l.starts_with("Exec=") && l.split_whitespace().any(|w| w == "--hidden")),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_value_quotes_the_program() {
        let exe = Path::new("C:\\Users\\j\\AppData\\Local\\grapple\\grapple.exe");
        assert_eq!(run_command(exe, true), "\"C:\\Users\\j\\AppData\\Local\\grapple\\grapple.exe\" --hidden");
        assert_eq!(run_command(exe, false), "\"C:\\Users\\j\\AppData\\Local\\grapple\\grapple.exe\"");
        assert!(run_command_hidden(Some(&run_command(exe, true))));
        assert!(!run_command_hidden(Some(&run_command(exe, false))));
        assert!(!run_command_hidden(Some("\"C:\\--hidden games\\grapple.exe\"")), "a folder name is not a flag");
        assert!(run_command_hidden(None));
    }

    #[cfg(not(windows))]
    #[test]
    fn entry_with_and_without_hidden() {
        let dir = std::env::temp_dir().join(format!("grapple-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("autostart").join("grapple.desktop");
        let exe = || Ok(PathBuf::from("/opt/my games/grapple"));

        write_entry(&path, true, true, exe).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=\"/opt/my games/grapple\" --hidden\n"), "{text}");
        assert!(entry_hidden(&path));

        write_entry(&path, true, false, exe).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=\"/opt/my games/grapple\"\n"), "{text}");
        assert!(!text.contains("--hidden"));
        assert!(!entry_hidden(&path));

        write_entry(&path, false, false, exe).unwrap();
        assert!(!path.exists());
        // Removing an entry that is not there is fine; no entry reads as the default (hidden).
        write_entry(&path, false, true, exe).unwrap();
        assert!(entry_hidden(&path));
        std::fs::remove_dir_all(&dir).ok();
    }
}
