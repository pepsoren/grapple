//! Epic's in-game overlay ("Epic Online Services Overlay"). Games such as Rocket League show their store checkout
//! in it; on Windows the Epic launcher installs it. grapple downloads it itself: it is an Epic app like any game,
//! so the same engine fetches it, into `<grapple data>/eos-overlay`, never registered in legendary's game list.
//! A new build goes to `eos-overlay.new` first and is swapped in whole. Switching it on per game happens in
//! `grapple_launch::eos`, just before the game starts.

use crate::engine::{self, Build, Control, EngineConfig, Job, Kind, Outcome, Progress};
use crate::epic::EpicClient;
use crate::error::{Error, Result};
use crate::legendary::EpicMeta;
use grapple_launch::plan::Secret;
use std::path::{Path, PathBuf};

/// The overlay's ids in Epic's catalog (it is an app in Epic's own namespace).
pub const APP_NAME: &str = "98bc04bc842e4906993fd6d6644ffb8d";
pub const NAMESPACE: &str = "302e5ede476149b1bc3e4fe6ae45e50e";
pub const CATALOG_ITEM: &str = "cc15684f44d849e89e9bf4cec0508b68";

/// grapple's record of the installed build, inside the overlay folder.
const VERSION_FILE: &str = ".grapple-overlay.json";

/// The installed build, when the folder holds a complete overlay.
pub fn installed_version(dir: &Path) -> Option<String> {
    if !dir.join(grapple_launch::eos::MARKER).is_file() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(VERSION_FILE)).ok()?).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{suffix}"));
    dir.with_file_name(name)
}

fn meta() -> EpicMeta {
    EpicMeta {
        app_name: APP_NAME.into(),
        title: "Epic Online Services Overlay".into(),
        namespace: NAMESPACE.into(),
        catalog_item_id: CATALOG_ITEM.into(),
        folder_name: None,
        can_run_offline: false,
        requires_ot: false,
    }
}

/// Install the overlay into `dir`, or update it to Epic's current build. Returns the build installed.
pub async fn install(cfg: &EngineConfig, epic: &EpicClient, token: &Secret, dir: &Path, legendary_dir: &Path) -> Result<String> {
    let loc = epic.manifest_location(token, NAMESPACE, CATALOG_ITEM, APP_NAME).await?;
    if installed_version(dir).as_deref() == Some(loc.build_version.as_str()) {
        return Ok(loc.build_version);
    }
    let bytes = epic.manifest(&loc).await?;
    let build = Build::new(bytes, loc.base_urls)?;
    let (new, old) = (sibling(dir, "new"), sibling(dir, "old"));
    for p in [&new, &old] {
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| Error::file(p, e))?;
        }
    }
    let job = Job { kind: Kind::Install, meta: meta(), install_path: new.clone(), legendary_dir: legendary_dir.to_path_buf(), register: false };
    let version = match engine::run(cfg, &job, Some(build), Control::new(), Progress::new()).await? {
        Outcome::Done { version, .. } => version,
        other => return Err(Error::Invalid(format!("the overlay download stopped ({other:?})"))),
    };
    let record = serde_json::json!({ "version": version });
    let vf = new.join(VERSION_FILE);
    std::fs::write(&vf, record.to_string()).map_err(|e| Error::file(&vf, e))?;
    // Swap the whole folder, so a game never sees half an overlay.
    if dir.exists() {
        std::fs::rename(dir, &old).map_err(|e| Error::file(dir, e))?;
    }
    std::fs::rename(&new, dir).map_err(|e| Error::file(dir, e))?;
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(version)
}
