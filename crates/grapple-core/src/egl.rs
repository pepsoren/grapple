//! Games installed by the Epic Games Launcher (Windows), read from its own files so they show up and start in
//! grapple without legendary or Heroic having installed them:
//!
//! * `%PROGRAMDATA%\Epic\EpicGamesLauncher\Data\Manifests\*.item`: one JSON file per installed app (`AppName`,
//!   `DisplayName`, `InstallLocation`, `LaunchExecutable`, `LaunchCommand`, `CatalogNamespace`, `CatalogItemId`,
//!   `bCanRunOffLine`, `OwnershipToken`, ...). legendary's EGL import reads the same files (`models/egl.py`).
//! * `%PROGRAMDATA%\Epic\UnrealEngineLauncher\LauncherInstalled.dat`: the launcher's install list (JSON, despite the
//!   name), for apps whose `.item` file is missing.
//!
//! Read-only: grapple never writes the launcher's files.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EglGame {
    pub app_name: String,
    pub display_name: String,
    pub install_location: PathBuf,
    /// Relative to `install_location` (`Binaries\Win64\Game.exe`); empty when only `LauncherInstalled.dat` knew it.
    pub launch_executable: String,
    /// Extra arguments the launcher passes (`LaunchCommand`).
    pub launch_command: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub can_run_offline: bool,
    /// The game wants an ownership token file (`-epicovt=`).
    pub requires_ot: bool,
    pub install_size: Option<u64>,
}

impl EglGame {
    /// The launcher's own link that starts this game (it shows the launcher, `silent=true` keeps it in the tray).
    pub fn launch_uri(&self) -> String {
        format!("com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true", self.namespace, self.catalog_item_id, self.app_name)
    }
}

/// Parse one `.item` manifest. `None` for DLC, add-ons, unfinished installs and anything that is not an app.
pub fn parse_item(bytes: &[u8]) -> Option<EglGame> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let app_name = s("AppName");
    if app_name.is_empty() || s("InstallLocation").is_empty() || b("bIsIncompleteInstall") {
        return None;
    }
    // DLC carry their base game's name in MainGameAppName.
    let main = s("MainGameAppName");
    if !main.is_empty() && main != app_name {
        return None;
    }
    // "OwnershipToken" is the string "true"/"false" in the launcher's files.
    let requires_ot = match v.get("OwnershipToken") {
        Some(Value::String(t)) => t.eq_ignore_ascii_case("true"),
        Some(Value::Bool(t)) => *t,
        _ => false,
    };
    Some(EglGame {
        display_name: Some(s("DisplayName")).filter(|n| !n.is_empty()).unwrap_or_else(|| app_name.clone()),
        install_location: PathBuf::from(s("InstallLocation")),
        launch_executable: s("LaunchExecutable"),
        launch_command: s("LaunchCommand"),
        namespace: s("CatalogNamespace"),
        catalog_item_id: s("CatalogItemId"),
        can_run_offline: b("bCanRunOffLine"),
        requires_ot,
        install_size: v.get("InstallSize").and_then(Value::as_u64),
        app_name,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Installed {
    #[serde(default)]
    installation_list: Vec<InstalledEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InstalledEntry {
    #[serde(default)]
    install_location: String,
    #[serde(default)]
    namespace_id: String,
    #[serde(default)]
    item_id: String,
    #[serde(default)]
    app_name: String,
}

/// Parse `LauncherInstalled.dat`. These entries have no executable; they only let a game appear (and start through
/// the launcher's link).
pub fn parse_launcher_installed(bytes: &[u8]) -> Vec<EglGame> {
    let Ok(list) = serde_json::from_slice::<Installed>(bytes) else { return Vec::new() };
    list.installation_list
        .into_iter()
        .filter(|e| !e.app_name.is_empty() && !e.install_location.is_empty())
        .map(|e| EglGame {
            display_name: e.app_name.clone(),
            install_location: PathBuf::from(e.install_location),
            namespace: e.namespace_id,
            catalog_item_id: e.item_id,
            app_name: e.app_name,
            ..EglGame::default()
        })
        .collect()
}

/// Where the launcher keeps its files: `%PROGRAMDATA%\Epic` (`C:\ProgramData\Epic`). `None` off Windows.
pub fn default_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let pd = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
    Some(pd.join("Epic"))
}

/// Every installed game the launcher knows, sorted by app name. `epic_dir` is [`default_dir`] (tests: a fixture).
pub fn installed(epic_dir: &Path) -> Vec<EglGame> {
    let mut out: Vec<EglGame> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(epic_dir.join("EpicGamesLauncher").join("Data").join("Manifests")) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("item")) {
                continue;
            }
            if let Some(g) = std::fs::read(&p).ok().and_then(|b| parse_item(&b)) {
                if !out.iter().any(|o| o.app_name == g.app_name) {
                    out.push(g);
                }
            }
        }
    }
    if let Ok(bytes) = std::fs::read(epic_dir.join("UnrealEngineLauncher").join("LauncherInstalled.dat")) {
        for g in parse_launcher_installed(&bytes) {
            if !out.iter().any(|o| o.app_name == g.app_name) {
                out.push(g);
            }
        }
    }
    out.sort_by(|a, b| a.app_name.cmp(&b.app_name));
    out
}

/// One installed game by app name.
pub fn find(epic_dir: &Path, app_name: &str) -> Option<EglGame> {
    installed(epic_dir).into_iter().find(|g| g.app_name == app_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = r#"{
        "FormatVersion": 0, "bIsIncompleteInstall": false, "AppVersionString": "1.0",
        "LaunchCommand": "-nosplash", "LaunchExecutable": "Binaries/Win64/RocketLeague.exe",
        "ManifestLocation": "C:\\ProgramData\\Epic\\EpicGamesLauncher\\Data\\Manifests",
        "bIsApplication": true, "bIsExecutable": true, "bIsManaged": false, "bNeedsValidation": false,
        "bRequiresAuth": true, "bCanRunOffLine": true, "DisplayName": "Rocket League®",
        "InstallLocation": "C:\\Program Files\\Epic Games\\rocketleague", "InstallSize": 21474836480,
        "CatalogNamespace": "9773aa1aa54f4f7b80e44bef04986cea", "CatalogItemId": "530145df28a24424923f5828cc9031a1",
        "AppName": "Sugar", "MainGameAppName": "Sugar", "OwnershipToken": "false", "AppCategories": ["public", "games", "applications"]
    }"#;

    #[test]
    fn reads_a_launcher_manifest() {
        let g = parse_item(ITEM.as_bytes()).unwrap();
        assert_eq!(g.app_name, "Sugar");
        assert_eq!(g.display_name, "Rocket League®");
        assert_eq!(g.install_location, PathBuf::from("C:\\Program Files\\Epic Games\\rocketleague"));
        assert_eq!(g.launch_executable, "Binaries/Win64/RocketLeague.exe");
        assert_eq!(g.launch_command, "-nosplash");
        assert!(g.can_run_offline && !g.requires_ot);
        assert_eq!(g.install_size, Some(21474836480));
        assert_eq!(
            g.launch_uri(),
            "com.epicgames.launcher://apps/9773aa1aa54f4f7b80e44bef04986cea%3A530145df28a24424923f5828cc9031a1%3ASugar?action=launch&silent=true"
        );
    }

    #[test]
    fn skips_dlc_unfinished_installs_and_junk() {
        let dlc = ITEM.replace(r#""AppName": "Sugar""#, r#""AppName": "SugarDLC""#);
        assert_eq!(parse_item(dlc.as_bytes()), None);
        let partial = ITEM.replace(r#""bIsIncompleteInstall": false"#, r#""bIsIncompleteInstall": true"#);
        assert_eq!(parse_item(partial.as_bytes()), None);
        assert_eq!(parse_item(b"not json"), None);
        let ot = ITEM.replace(r#""OwnershipToken": "false""#, r#""OwnershipToken": "true""#);
        assert!(parse_item(ot.as_bytes()).unwrap().requires_ot);
    }

    #[test]
    fn lists_manifests_and_the_install_list() {
        let dir = tempfile::tempdir().unwrap();
        let manifests = dir.path().join("EpicGamesLauncher").join("Data").join("Manifests");
        std::fs::create_dir_all(&manifests).unwrap();
        std::fs::write(manifests.join("A1B2.item"), ITEM).unwrap();
        std::fs::write(manifests.join("notes.txt"), "x").unwrap();
        let uel = dir.path().join("UnrealEngineLauncher");
        std::fs::create_dir_all(&uel).unwrap();
        std::fs::write(
            uel.join("LauncherInstalled.dat"),
            r#"{"InstallationList":[
                {"InstallLocation":"C:\\Program Files\\Epic Games\\rocketleague","NamespaceId":"ns","ItemId":"it","ArtifactId":"Sugar","AppVersion":"1","AppName":"Sugar"},
                {"InstallLocation":"D:\\Epic\\Fortnite","NamespaceId":"fn","ItemId":"4fe7","ArtifactId":"Fortnite","AppVersion":"2","AppName":"Fortnite"}
            ]}"#,
        )
        .unwrap();
        let all = installed(dir.path());
        assert_eq!(all.iter().map(|g| g.app_name.as_str()).collect::<Vec<_>>(), ["Fortnite", "Sugar"]);
        assert_eq!(all[1].launch_executable, "Binaries/Win64/RocketLeague.exe", "the manifest wins over the install list");
        assert_eq!(all[0].launch_executable, "");
        assert_eq!(all[0].install_location, PathBuf::from("D:\\Epic\\Fortnite"));
        assert_eq!(find(dir.path(), "Fortnite").map(|g| g.namespace), Some("fn".into()));
        assert!(installed(&dir.path().join("missing")).is_empty());
    }
}
