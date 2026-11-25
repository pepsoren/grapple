//! What Steam's own files say about Josh's account. Read-only; cookies, tokens and passwords are never opened
//! (`config/htmlcache` holds the embedded browser's cookies and is not touched).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The account id: the numeric folder in `userdata/` (the one named in `loginusers.vdf` as most recent, else the
/// newest `localconfig.vdf`).
pub fn account_id(steam_root: &Path) -> Option<u32> {
    let ud = steam_root.join("userdata");
    let mut ids: Vec<(std::time::SystemTime, u32)> = std::fs::read_dir(&ud)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let id: u32 = e.file_name().to_str()?.parse().ok()?;
            if id == 0 {
                return None;
            }
            let t = std::fs::metadata(e.path().join("config/localconfig.vdf")).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            Some((t, id))
        })
        .collect();
    ids.sort();
    ids.last().map(|(_, id)| *id)
}

/// A two-letter country code from a line like `"country"  "ZA"` (any key containing `country`, any case).
fn vdf_country(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split('"').filter(|s| !s.trim().is_empty());
        let (Some(k), Some(v)) = (parts.next(), parts.next()) else { continue };
        if k.to_ascii_lowercase().contains("country") && v.len() == 2 && v.bytes().all(|b| b.is_ascii_uppercase()) {
            return Some(v.to_string());
        }
    }
    None
}

fn tail(path: &Path, bytes: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(bytes))).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The last `COUNTRY=XX` Steam's UI logged (the country its client saw at login).
fn log_country(text: &str) -> Option<String> {
    let mut found = None;
    let mut rest = text;
    while let Some(i) = rest.find("COUNTRY=") {
        rest = &rest[i + 8..];
        let c: String = rest.chars().take(2).collect();
        if c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()) {
            found = Some(c);
        }
    }
    found
}

/// Steam's idea of the store country: the account's `country` in its config files first, then what Steam's UI
/// logged. `None` when neither names one.
pub fn detect_country(steam_root: &Path) -> Option<(String, &'static str)> {
    let mut files: Vec<PathBuf> = vec![steam_root.join("config/config.vdf"), steam_root.join("config/loginusers.vdf")];
    if let Some(id) = account_id(steam_root) {
        files.push(steam_root.join(format!("userdata/{id}/config/localconfig.vdf")));
    }
    for f in files {
        // Files over 4 MB are not these.
        if std::fs::metadata(&f).map(|m| m.len() > 4_000_000).unwrap_or(true) {
            continue;
        }
        if let Some(c) = std::fs::read_to_string(&f).ok().and_then(|t| vdf_country(&t)) {
            return Some((c, "steam-config"));
        }
    }
    for log in ["logs/webhelper_js.txt", "logs/cef_log.txt"] {
        if let Some(c) = tail(&steam_root.join(log), 512 * 1024).and_then(|t| log_country(&t)) {
            return Some((c, "steam-log"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("userdata/12345678/config")).unwrap();
        std::fs::create_dir_all(d.path().join("userdata/0/config")).unwrap();
        std::fs::create_dir_all(d.path().join("config")).unwrap();
        std::fs::create_dir_all(d.path().join("logs")).unwrap();
        d
    }

    #[test]
    fn account_id_is_the_numeric_userdata_folder() {
        let d = root();
        assert_eq!(account_id(d.path()), Some(12345678));
        assert_eq!(account_id(&d.path().join("nope")), None);
    }

    #[test]
    fn country_from_config_then_logs() {
        let d = root();
        assert_eq!(detect_country(d.path()), None);
        std::fs::write(d.path().join("logs/webhelper_js.txt"), "x ?LANGUAGE=english&COUNTRY=&A=1\nx ?COUNTRY=DE&LAUNCHER_TYPE=0\n").unwrap();
        assert_eq!(detect_country(d.path()), Some(("DE".into(), "steam-log")));
        std::fs::write(d.path().join("userdata/12345678/config/localconfig.vdf"), "\"FamilyGroup\"\n{\n\t\"country\"\t\t\"\"\n}\n").unwrap();
        assert_eq!(detect_country(d.path()), Some(("DE".into(), "steam-log")), "an empty country is not an answer");
        std::fs::write(d.path().join("userdata/12345678/config/localconfig.vdf"), "\"FamilyGroup\"\n{\n\t\"country\"\t\t\"ZA\"\n}\n").unwrap();
        assert_eq!(detect_country(d.path()), Some(("ZA".into(), "steam-config")));
    }

    #[test]
    fn parsers_ignore_junk() {
        assert_eq!(vdf_country("\"country\" \"zz\""), None);
        assert_eq!(vdf_country("\"IPCountry\" \"GB\""), Some("GB".into()));
        assert_eq!(log_country("COUNTRY=\nCOUNTRY=DE\nCOUNTRY=\n"), Some("DE".into()));
    }
}
