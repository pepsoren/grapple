// SPDX-License-Identifier: GPL-3.0-only
// Ported from Heroic Games Launcher (GPL-3.0-only); see NOTICE.md.
//! Reading Heroic's configuration (no secrets in any of these files) and turning it into a Proton/Wine runtime
//! the way Heroic does. Ported from HeroicGamesLauncher (main branch):
//! * `src/backend/launcher.ts` `setupEnvVars` (L1029), `setupWrapperEnvVars` (L1072), `setupWineEnvVars` (L1110),
//!   `setupWrappers` (L1321), `prepareLaunch` umu choice (L713-726),
//! * `src/backend/utils/compatibility_layers.ts` `getWineFlags` (L518), `isUmuSupported` (L581),
//! * `src/backend/storeManagers/legendary/games.ts` `launch` (L881): wrappers + `--no-wine`.

use crate::error::{LaunchError, Result};
use crate::paths;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WineVersion {
    #[serde(default)]
    pub bin: String,
    #[serde(default)]
    pub name: String,
    /// "proton", "wine", ...
    #[serde(default, rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EnvOption {
    pub key: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WrapperOption {
    pub exe: String,
    #[serde(default)]
    pub args: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LaunchOption {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub parameters: String,
}

fn yes() -> bool {
    true
}

/// A game's effective Heroic settings: global `defaultSettings` overridden by `GamesConfig/<app>.json`
/// (Heroic does `{...global, ...game}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HeroicSettings {
    #[serde(rename = "wineVersion")]
    pub wine_version: WineVersion,
    #[serde(rename = "winePrefix")]
    pub wine_prefix: String,
    #[serde(rename = "useGameMode")]
    pub use_game_mode: bool,
    #[serde(rename = "enviromentOptions")]
    pub env_options: Vec<EnvOption>,
    #[serde(rename = "wrapperOptions")]
    pub wrapper_options: Vec<WrapperOption>,
    #[serde(rename = "launcherArgs")]
    pub launcher_args: String,
    #[serde(rename = "targetExe")]
    pub target_exe: String,
    pub language: String,
    #[serde(rename = "offlineMode")]
    pub offline_mode: bool,
    #[serde(rename = "disableUMU")]
    pub disable_umu: bool,
    #[serde(rename = "enableEsync")]
    pub esync: bool,
    #[serde(rename = "enableFsync")]
    pub fsync: bool,
    #[serde(rename = "enableWineWayland")]
    pub wayland: bool,
    #[serde(rename = "enableHDR")]
    pub hdr: bool,
    #[serde(rename = "enableWoW64")]
    pub wow64: bool,
    #[serde(rename = "autoInstallDxvkNvapi")]
    pub nvapi: bool,
    #[serde(rename = "eacRuntime")]
    pub eac_runtime: bool,
    #[serde(rename = "battlEyeRuntime")]
    pub battleye_runtime: bool,
    #[serde(rename = "nvidiaPrime")]
    pub nvidia_prime: bool,
    #[serde(rename = "showFps")]
    pub show_fps: bool,
    #[serde(rename = "lastUsedLaunchOption")]
    pub launch_option: Option<LaunchOption>,
}

impl Default for HeroicSettings {
    fn default() -> Self {
        HeroicSettings {
            wine_version: WineVersion::default(),
            wine_prefix: String::new(),
            use_game_mode: false,
            env_options: Vec::new(),
            wrapper_options: Vec::new(),
            launcher_args: String::new(),
            target_exe: String::new(),
            language: String::new(),
            offline_mode: false,
            disable_umu: false,
            esync: yes(),
            fsync: yes(),
            wayland: false,
            hdr: false,
            wow64: false,
            nvapi: yes(),
            eac_runtime: yes(),
            battleye_runtime: yes(),
            nvidia_prime: false,
            show_fps: false,
            launch_option: None,
        }
    }
}

impl HeroicSettings {
    /// Load from a Heroic config directory (`~/.config/heroic`). Missing files are not an error: defaults apply.
    pub fn load(heroic_dir: &Path, app_name: &str) -> Result<HeroicSettings> {
        let mut merged = serde_json::Map::new();
        if let Some(v) = read_json_opt(&heroic_dir.join("config.json"))? {
            if let Some(Value::Object(d)) = v.get("defaultSettings") {
                merged.extend(d.clone());
            }
        }
        if let Some(v) = read_json_opt(&heroic_dir.join("GamesConfig").join(format!("{app_name}.json")))? {
            if let Some(Value::Object(g)) = v.get(app_name) {
                merged.extend(g.clone());
            }
        }
        serde_json::from_value(Value::Object(merged))
            .map_err(|source| LaunchError::Parse { path: heroic_dir.join("GamesConfig").join(format!("{app_name}.json")), source })
    }

    pub fn extra_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(o) = &self.launch_option {
            // Heroic only passes basic launch options' parameters (games.ts:995)
            if (o.kind.is_empty() || o.kind == "basic") && !o.parameters.trim().is_empty() {
                out.extend(shlex_split(&o.parameters));
            }
        }
        if !self.launcher_args.trim().is_empty() {
            out.extend(shlex_split(&self.launcher_args));
        }
        out
    }
}

fn read_json_opt(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|source| LaunchError::Parse { path: path.to_path_buf(), source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(LaunchError::Read { path: path.to_path_buf(), source }),
    }
}

/// POSIX-ish word splitting (python `shlex.split`) for Heroic's launcher args and wrapper args.
pub fn shlex_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' && matches!(chars.peek(), Some('"') | Some('\\') | Some('$') | Some('`')) {
                    cur.push(chars.next().unwrap());
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                        in_word = true;
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        out.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// Heroic `removeQuoteIfNecessary`
fn unquote(v: &str) -> String {
    let t = v.trim();
    if t.len() >= 2 && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\''))) {
        return t[1..t.len() - 1].to_string();
    }
    v.to_string()
}

/// Where Heroic keeps its tools: `~/.config/heroic/tools/runtimes` (constants/paths.ts:26).
pub fn runtimes_dir(heroic_dir: &Path) -> PathBuf {
    heroic_dir.join("tools").join("runtimes")
}

/// Heroic's `getUmuPath`: `umu-run` on `PATH`, else its own `runtimes/umu/umu_run.py`.
/// `override_path` (a Grapple setting) wins; `disable` follows Heroic's `disableUMU`.
pub fn find_umu(override_path: Option<&Path>, heroic_dir: &Path, path_var: &str, disable: bool) -> Option<PathBuf> {
    if disable {
        return None;
    }
    if let Some(p) = override_path {
        return p.exists().then(|| p.to_path_buf());
    }
    if let Some(p) = paths::which_in("umu-run", path_var) {
        return Some(p);
    }
    let own = runtimes_dir(heroic_dir).join("umu").join("umu_run.py");
    own.exists().then_some(own)
}

/// Heroic caches umu ids it looked up in `store_cache/umu.json` (`legendary_<app>` -> `umu-252950`).
/// We only read that cache (no network); an unknown game gets `umu-0` like Heroic's default.
pub fn cached_umu_id(heroic_dir: &Path, key: &str) -> Option<String> {
    let v = read_json_opt(&heroic_dir.join("store_cache").join("umu.json")).ok()??;
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The plan note of a launch through umu-run (the launcher then follows umu's output, see [`crate::umu`]).
pub const NOTE_UMU: &str = "runtime: umu-run (Steam Linux Runtime container, like Heroic)";

/// Everything about how to run a Windows program: wrappers, runner, environment.
#[derive(Debug, Clone)]
pub struct WindowsRuntime {
    /// Wrapper commands, outermost first (`gamemoderun`, user wrappers, then umu).
    pub argv_prefix: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub notes: Vec<String>,
}

pub struct RuntimeInput<'a> {
    pub settings: &'a HeroicSettings,
    pub heroic_dir: &'a Path,
    pub umu_override: Option<&'a Path>,
    pub path_var: &'a str,
    /// `~/.steam/steam`
    pub steam_client_path: &'a Path,
    pub install_path: Option<&'a Path>,
    /// `heroic-<folder name>` for `SteamGameId` (launcher.ts:1235)
    pub steam_game_id: String,
    /// umu `GAMEID` (`umu-252950` or `umu-0`)
    pub umu_game_id: String,
    /// umu `STORE` (`egs` for Epic), None for non-store games
    pub umu_store: Option<&'static str>,
    /// extra env that identifies the game to Grapple's tracker, and Heroic-style app markers
    pub extra_env: BTreeMap<String, String>,
    /// GameMode as [`crate::gamemode::choose`] decided: only `Wrap` puts `gamemoderun` in front of umu.
    pub gamemode: crate::gamemode::Via,
    /// umu's folder (its Steam Linux Runtimes), see [`crate::umu::default_dir`].
    pub umu_dir: &'a Path,
    /// The overlay was asked for; `mangohud_bin` is where `mangohud` is (None = not installed).
    pub mangohud: bool,
    pub mangohud_bin: Option<PathBuf>,
    /// How controllers are shown to the game (Proton only, see [`crate::pads`]).
    pub pad_mode: crate::pads::PadMode,
}

/// Build the runtime. Fails with a clear message when the configured Proton/Wine is missing.
pub fn windows_runtime(i: &RuntimeInput<'_>) -> Result<WindowsRuntime> {
    let s = i.settings;
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let mut notes = Vec::new();

    // launcher.ts setupEnvVars
    if s.nvidia_prime {
        env.insert("DRI_PRIME".into(), "1".into());
        env.insert("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into());
        env.insert("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into());
    }
    if let Some(p) = i.install_path {
        env.insert("STEAM_COMPAT_INSTALL_PATH".into(), p.to_string_lossy().into_owned());
    }
    for e in &s.env_options {
        env.insert(e.key.clone(), unquote(&e.value));
    }
    // "setup LD_PRELOAD if not defined": fixes std::log_error for Fall Guys (launcher.ts:1055)
    if std::env::var_os("LD_PRELOAD").is_none() && !env.contains_key("LD_PRELOAD") {
        env.insert("LD_PRELOAD".into(), String::new());
    }

    let prefix = s.wine_prefix.replace('\'', "");
    if prefix.is_empty() {
        return Err(LaunchError::Config("this game has no Wine prefix in Heroic; open it once in Heroic first".into()));
    }
    let wine_bin = s.wine_version.bin.replace('\'', "");
    if wine_bin.is_empty() {
        return Err(LaunchError::Config("this game has no Proton/Wine version in Heroic".into()));
    }
    if !Path::new(&wine_bin).exists() {
        return Err(LaunchError::Missing { what: "the configured Proton/Wine", detail: wine_bin });
    }

    let mut runner: Vec<String> = Vec::new();
    let is_proton = s.wine_version.kind == "proton";
    if is_proton {
        // launcher.ts setupWineEnvVars, case 'proton' (L1136-1141)
        env.insert("STEAM_COMPAT_CLIENT_INSTALL_PATH".into(), i.steam_client_path.to_string_lossy().into_owned());
        env.insert("WINEPREFIX".into(), prefix.clone());
        env.insert("STEAM_COMPAT_DATA_PATH".into(), prefix.clone());
        let proton_dir = Path::new(&wine_bin).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        env.insert("PROTONPATH".into(), proton_dir);
        if !s.esync {
            env.insert("PROTON_NO_ESYNC".into(), "1".into());
        }
        if !s.fsync {
            env.insert("PROTON_NO_FSYNC".into(), "1".into());
        }
        if s.wayland {
            env.insert("PROTON_ENABLE_WAYLAND".into(), "1".into());
            if s.hdr {
                env.insert("PROTON_ENABLE_HDR".into(), "1".into());
            }
        }
        if s.wow64 {
            env.insert("PROTON_USE_WOW64".into(), "1".into());
        }
        // After Heroic's environment options, so a value set there (or in the session) stays.
        crate::pads::apply(i.pad_mode, &mut env, |k| crate::env::child_var(k).is_some());
        if s.nvapi {
            env.insert("PROTON_ENABLE_NVAPI".into(), "1".into());
            env.insert("DXVK_NVAPI_ALLOW_OTHER_DRIVERS".into(), "1".into());
        } else {
            env.insert("PROTON_DISABLE_NVAPI".into(), "1".into());
        }
        let runtimes = runtimes_dir(i.heroic_dir);
        if s.eac_runtime {
            env.insert("PROTON_EAC_RUNTIME".into(), runtimes.join("eac_runtime").to_string_lossy().into_owned());
        }
        if s.battleye_runtime {
            env.insert("PROTON_BATTLEYE_RUNTIME".into(), runtimes.join("battleye_runtime").to_string_lossy().into_owned());
        }
        // "If we don't set this, GE-Proton tries to guess the AppID from the prefix path" (L1230)
        env.insert("STEAM_COMPAT_APP_ID".into(), "0".into());
        env.insert("SteamAppId".into(), "0".into());
        env.insert("SteamGameId".into(), i.steam_game_id.clone());
    } else {
        // plain Wine (setupWineEnvVars case 'wine')
        env.insert("WINEPREFIX".into(), prefix.clone());
        env.insert("WINEDLLOVERRIDES".into(), "winemenubuilder.exe=d".into());
        if s.esync {
            env.insert("WINEESYNC".into(), "1".into());
        }
        if s.fsync {
            env.insert("WINEFSYNC".into(), "1".into());
        }
    }
    if s.show_fps {
        env.insert("DXVK_HUD".into(), "fps".into());
    }

    // wrapper env vars (setupWrapperEnvVars L1072)
    env.insert("GAMEID".into(), i.umu_game_id.clone());
    if let Some(store) = i.umu_store {
        env.insert("STORE".into(), store.into());
    }
    for (k, v) in &i.extra_env {
        env.insert(k.clone(), v.clone());
    }

    // wrappers (setupWrappers L1321): user wrappers, then gamemode, then the umu runtime.
    let mut wrappers: Vec<String> = Vec::new();
    for w in &s.wrapper_options {
        wrappers.push(w.exe.clone());
        wrappers.extend(shlex_split(&w.args));
    }
    match &i.gamemode {
        crate::gamemode::Via::Wrap(g) => wrappers.push(g.to_string_lossy().into_owned()),
        crate::gamemode::Via::Register(_) => notes.push("GameMode: the game's own processes are registered with gamemoded once they run".into()),
        crate::gamemode::Via::Off if s.use_game_mode => notes.push("GameMode was requested but is not installed; starting without it".into()),
        crate::gamemode::Via::Off => {}
    }
    if i.mangohud {
        match &i.mangohud_bin {
            // Heroic wraps with `mangohud --dlsym` (OpenGL games under Wine need the dlsym hook).
            Some(m) => {
                wrappers.push(m.to_string_lossy().into_owned());
                wrappers.push("--dlsym".into());
            }
            None => notes.push("MangoHud was requested but is not installed; starting without it".into()),
        }
    }
    if is_proton {
        match find_umu(i.umu_override, i.heroic_dir, i.path_var, s.disable_umu) {
            Some(umu) => {
                wrappers.push(umu.to_string_lossy().into_owned());
                notes.push(NOTE_UMU.into());
                // Never wait for umu's runtime download when a complete runtime is installed.
                let proton_dir = Path::new(&wine_bin).parent().unwrap_or(Path::new(""));
                crate::umu::apply(crate::umu::runtime_check(proton_dir, i.umu_dir), &mut env, &mut notes);
            }
            None => {
                // compatibility_layers.ts:538-545: without umu the wrapper is `<proton> waitforexitandrun`
                runner.push(wine_bin.clone());
                runner.push("waitforexitandrun".into());
                notes.push("runtime: Proton script directly (umu-run not available)".into());
            }
        }
    } else {
        runner.push(wine_bin.clone());
        notes.push("runtime: Wine".into());
    }
    let mut argv_prefix = wrappers;
    argv_prefix.extend(runner);
    Ok(WindowsRuntime { argv_prefix, env, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_basics() {
        assert_eq!(shlex_split(r#"-a "b c" 'd e' f\ g"#), ["-a", "b c", "d e", "f g"]);
        assert!(shlex_split("   ").is_empty());
        assert_eq!(shlex_split(r#""""#), [""]);
    }

    #[test]
    fn unquote_works() {
        assert_eq!(unquote("\"x y\""), "x y");
        assert_eq!(unquote("x"), "x");
    }
}
