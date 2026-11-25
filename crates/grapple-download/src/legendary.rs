//! The files legendary (and Heroic, which uses it) keep, read and written the way they read them, so a game grapple
//! installs is a normal installed game for grapple-scan, grapple-launch and Heroic. Implemented from the format
//! description in `docs/DOWNLOADS_PLAN.md`; no legendary code.
//!
//! * `installed.json` is changed only under `installed.json.lock` (the lock legendary takes), read-modify-write,
//!   written atomically, every other game and every unknown field kept.
//! * `manifests/<app>_<platform>_<build>.manifest` holds the raw manifest of an installed build (old ones stay).

use crate::error::{Error, Result};
use crate::manifest::Manifest;
use grapple_launch::epic_auth::{FileLock, write_atomic};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PLATFORM: &str = "Windows";

/// legendary's app names: letters, digits, `_`, `-`.
pub fn valid_app_name(s: &str) -> bool {
    crate::epic::valid_id(s)
}

/// What grapple needs from `metadata/<app>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpicMeta {
    pub app_name: String,
    pub title: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub folder_name: Option<String>,
    pub can_run_offline: bool,
    pub requires_ot: bool,
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| Error::Format(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::file(path, e)),
    }
}

pub fn read_metadata(legendary_dir: &Path, app_name: &str) -> Result<EpicMeta> {
    if !valid_app_name(app_name) {
        return Err(Error::Invalid("not an Epic game".into()));
    }
    let v = read_json(&legendary_dir.join("metadata").join(format!("{app_name}.json")))?
        .ok_or_else(|| Error::Invalid("Epic has no details for this game yet; open Heroic once".into()))?;
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    let attr = |k: &str| s(&format!("/metadata/customAttributes/{k}/value"));
    let platform = v.pointer("/asset_infos/Windows");
    let ps = |k: &str| platform.and_then(|p| p.get(k)).and_then(Value::as_str).map(str::to_string);
    Ok(EpicMeta {
        app_name: app_name.to_string(),
        title: s("/app_title").or_else(|| s("/metadata/title")).unwrap_or_else(|| app_name.to_string()),
        namespace: ps("namespace").or_else(|| s("/metadata/namespace")).unwrap_or_default(),
        catalog_item_id: ps("catalog_item_id").or_else(|| s("/metadata/id")).unwrap_or_default(),
        folder_name: attr("FolderName").filter(|f| !f.trim().is_empty()),
        can_run_offline: attr("CanRunOffline").is_some_and(|v| v.eq_ignore_ascii_case("true")),
        requires_ot: attr("OwnershipToken").is_some_and(|v| v.eq_ignore_ascii_case("true")),
    })
}

/// The game's folder name: `FolderName` from the metadata, else the title without characters Windows refuses.
pub fn folder_name(meta: &EpicMeta) -> String {
    let raw = meta.folder_name.clone().unwrap_or_else(|| meta.title.clone());
    let clean: String = raw.chars().filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control()).collect();
    let clean = clean.trim().trim_end_matches('.').trim().to_string();
    if clean.is_empty() || clean == ".." { meta.app_name.clone() } else { clean }
}

/// `installed.json` as a map (an absent file is an empty map).
pub fn read_installed(legendary_dir: &Path) -> Result<Map<String, Value>> {
    Ok(match read_json(&legendary_dir.join("installed.json"))? {
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Error::Format("installed.json is not an object".into())),
        None => Map::new(),
    })
}

pub fn installed_entry(legendary_dir: &Path, app_name: &str) -> Result<Option<Map<String, Value>>> {
    Ok(read_installed(legendary_dir)?.get(app_name).and_then(Value::as_object).cloned())
}

/// The fields grapple sets for an installed build. On an update they overwrite these keys of the existing entry;
/// every other key stays.
pub fn entry_fields(m: &Manifest, meta: &EpicMeta, install_path: &Path, base_urls: &[String]) -> Map<String, Value> {
    let md = &m.meta;
    let prereq = if md.prereq_ids.is_empty() && md.prereq_path.is_empty() {
        Value::Null
    } else {
        json!({ "args": md.prereq_args, "ids": md.prereq_ids, "name": md.prereq_name, "path": md.prereq_path })
    };
    let uninstaller = if md.uninstall_action_path.is_empty() {
        Value::Null
    } else {
        json!({ "args": md.uninstall_action_args, "path": md.uninstall_action_path })
    };
    let v = json!({
        "app_name": meta.app_name,
        "base_urls": base_urls,
        "can_run_offline": meta.can_run_offline,
        "executable": md.launch_exe,
        "install_path": install_path.to_string_lossy(),
        "install_size": m.install_size(),
        "launch_parameters": md.launch_command,
        "platform": PLATFORM,
        "prereq_info": prereq,
        "requires_ot": meta.requires_ot,
        "title": meta.title,
        "uninstaller": uninstaller,
        "version": md.build_version,
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// A complete new entry, with legendary's defaults for everything grapple does not know.
pub fn new_entry(fields: Map<String, Value>) -> Map<String, Value> {
    let mut e = match json!({
        "egl_guid": "", "install_tags": [], "is_dlc": false, "is_preloaded": false, "manifest_path": null,
        "needs_verification": false, "save_path": null, "save_timestamp": null, "use_signed_url": false,
    }) {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    e.extend(fields);
    e
}

fn lock(legendary_dir: &Path) -> Result<FileLock> {
    std::fs::create_dir_all(legendary_dir).map_err(|e| Error::file(legendary_dir, e))?;
    FileLock::acquire(&legendary_dir.join("installed.json.lock"), Duration::from_secs(15)).map_err(Error::from)
}

fn write_installed(legendary_dir: &Path, all: &Map<String, Value>) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(all.clone())).map_err(|e| Error::Format(e.to_string()))?;
    bytes.push(b'\n');
    write_atomic(&legendary_dir.join("installed.json"), &bytes)?;
    Ok(())
}

/// Add the game, or merge `fields` into its entry. Blocking (takes the lock).
pub fn upsert_installed(legendary_dir: &Path, app_name: &str, fields: Map<String, Value>) -> Result<()> {
    let _l = lock(legendary_dir)?;
    let mut all = read_installed(legendary_dir)?;
    let entry = match all.remove(app_name) {
        Some(Value::Object(mut old)) => {
            old.extend(fields);
            old
        }
        _ => new_entry(fields),
    };
    all.insert(app_name.to_string(), Value::Object(entry));
    write_installed(legendary_dir, &all)
}

/// Remove the game's entry; returns it. Blocking.
pub fn remove_installed(legendary_dir: &Path, app_name: &str) -> Result<Option<Value>> {
    let _l = lock(legendary_dir)?;
    let mut all = read_installed(legendary_dir)?;
    let old = all.remove(app_name);
    if old.is_some() {
        write_installed(legendary_dir, &all)?;
    }
    Ok(old)
}

pub fn manifest_path(legendary_dir: &Path, app_name: &str, build_version: &str) -> Result<PathBuf> {
    if !valid_app_name(app_name) || build_version.is_empty() || build_version.contains(['/', '\\', '\0']) || build_version.contains("..") {
        return Err(Error::Invalid("bad build name".into()));
    }
    Ok(legendary_dir.join("manifests").join(format!("{app_name}_{PLATFORM}_{build_version}.manifest")))
}

pub fn write_manifest_file(legendary_dir: &Path, app_name: &str, build_version: &str, bytes: &[u8]) -> Result<PathBuf> {
    let p = manifest_path(legendary_dir, app_name, build_version)?;
    let dir = p.parent().expect("has a parent");
    std::fs::create_dir_all(dir).map_err(|e| Error::file(dir, e))?;
    std::fs::write(&p, bytes).map_err(|e| Error::file(&p, e))?;
    Ok(p)
}

/// Heroic's default install folder (`config.json` `defaultSettings.defaultInstallPath`).
pub fn heroic_default_install_path(heroic_dir: &Path) -> Option<PathBuf> {
    let v = read_json(&heroic_dir.join("config.json")).ok()??;
    let p = v.pointer("/defaultSettings/defaultInstallPath")?.as_str()?.trim();
    let p = p.trim_matches('\'');
    (!p.is_empty()).then(|| PathBuf::from(p))
}

/// legendary's `assets.json`: every owned game's build, for update checks while offline.
pub fn read_assets(legendary_dir: &Path) -> Vec<crate::epic::Asset> {
    let Ok(Some(v)) = read_json(&legendary_dir.join("assets.json")) else { return Vec::new() };
    v.get(PLATFORM).and_then(|w| serde_json::from_value(w.clone()).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names() {
        let mut m = EpicMeta { app_name: "Sugar".into(), title: "Rocket League®: <Ultimate>?".into(), ..EpicMeta::default() };
        assert_eq!(folder_name(&m), "Rocket League® Ultimate");
        m.folder_name = Some("rocketleague".into());
        assert_eq!(folder_name(&m), "rocketleague");
        m.folder_name = None;
        m.title = "...".into();
        assert_eq!(folder_name(&m), "Sugar");
    }

    #[test]
    fn manifest_names() {
        let p = manifest_path(Path::new("/l"), "Sugar", "++Prime+Update60-CL-529371").unwrap();
        assert_eq!(p, Path::new("/l/manifests/Sugar_Windows_++Prime+Update60-CL-529371.manifest"));
        assert!(manifest_path(Path::new("/l"), "Sugar", "../x").is_err());
        assert!(manifest_path(Path::new("/l"), "a/b", "1").is_err());
    }
}
