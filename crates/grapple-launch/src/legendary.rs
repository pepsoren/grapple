// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from legendary (GPLv3); see NOTICE.md.
//! Legendary's installed-game and metadata files (no secrets in either) and the exact game argument list.
//! Ported from legendary (master): `legendary/core.py` `get_launch_parameters` and `legendary/cli.py` `launch_game`.

use crate::epic_auth::Identity;
use crate::error::{LaunchError, Result};
use crate::heroic::shlex_split;
use crate::plan::PlanArg;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One entry of `installed.json` (legendary/models/game.py `InstalledGame`).
#[derive(Debug, Clone, Deserialize)]
pub struct InstalledGame {
    pub app_name: String,
    #[serde(default)]
    pub title: String,
    pub install_path: PathBuf,
    #[serde(default)]
    pub executable: String,
    #[serde(default)]
    pub launch_parameters: String,
    #[serde(default)]
    pub can_run_offline: bool,
    #[serde(default)]
    pub requires_ot: bool,
    #[serde(default)]
    pub platform: String,
}

impl InstalledGame {
    /// A game the Epic Games Launcher installed, as legendary's EGL import sees it (`core.py` `egl_import`).
    pub fn from_egl(g: &grapple_core::egl::EglGame) -> InstalledGame {
        InstalledGame {
            app_name: g.app_name.clone(),
            title: g.display_name.clone(),
            install_path: g.install_location.clone(),
            executable: g.launch_executable.clone(),
            launch_parameters: g.launch_command.clone(),
            can_run_offline: g.can_run_offline,
            requires_ot: g.requires_ot,
            platform: "Windows".into(),
        }
    }

    pub fn load(legendary_dir: &Path, app_name: &str) -> Result<InstalledGame> {
        let path = legendary_dir.join("installed.json");
        let bytes = std::fs::read(&path).map_err(|source| LaunchError::Read { path: path.clone(), source })?;
        let all: Value = serde_json::from_slice(&bytes).map_err(|source| LaunchError::Parse { path: path.clone(), source })?;
        let entry = all.get(app_name).ok_or_else(|| LaunchError::NotInstalled(app_name.to_string()))?;
        serde_json::from_value(entry.clone()).map_err(|source| LaunchError::Parse { path, source })
    }
}

/// The bits of `metadata/<app>.json` a launch uses (legendary/models/game.py `Game`).
#[derive(Debug, Clone, Default)]
pub struct GameMeta {
    /// `metadata.namespace` -> `-epicsandboxid`
    pub namespace: String,
    /// `metadata.id`, for the ownership token
    pub catalog_item_id: String,
    /// `metadata.customAttributes.AdditionalCommandLine.value` (game.py:158)
    pub additional_command_line: Option<String>,
    /// `sidecar.config.deploymentId` -> `-epicdeploymentid`
    pub deployment_id: Option<String>,
}

impl GameMeta {
    /// What the Epic Games Launcher's manifest says (no extra command line or deployment id: those come from
    /// legendary's metadata, which is used instead when it exists).
    pub fn from_egl(g: &grapple_core::egl::EglGame) -> GameMeta {
        GameMeta { namespace: g.namespace.clone(), catalog_item_id: g.catalog_item_id.clone(), ..GameMeta::default() }
    }

    pub fn load(legendary_dir: &Path, app_name: &str) -> Result<GameMeta> {
        let path = legendary_dir.join("metadata").join(format!("{app_name}.json"));
        let bytes = std::fs::read(&path).map_err(|source| LaunchError::Read { path: path.clone(), source })?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|source| LaunchError::Parse { path, source })?;
        let meta = v.get("metadata");
        let text = |p: Option<&Value>| p.and_then(Value::as_str).map(str::to_string);
        Ok(GameMeta {
            namespace: text(meta.and_then(|m| m.get("namespace"))).unwrap_or_default(),
            catalog_item_id: text(meta.and_then(|m| m.get("id"))).unwrap_or_default(),
            additional_command_line: text(
                meta.and_then(|m| m.get("customAttributes")).and_then(|c| c.get("AdditionalCommandLine")).and_then(|a| a.get("value")),
            ),
            deployment_id: text(v.get("sidecar").and_then(|s| s.get("config")).and_then(|c| c.get("deploymentId"))),
        })
    }
}

/// Python `shlex.split(s, posix=False)` as used for the stored launch parameters (core.py:869, :877):
/// quotes stay in the token, a token that starts with a quote ends at the matching quote.
pub fn shlex_split_non_posix(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => {
                if c.is_whitespace() {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else if (c == '"' || c == '\'') && cur.is_empty() {
                    quote = Some(c);
                    cur.push(c);
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What the argument list needs besides the files.
pub struct ArgInputs<'a> {
    pub app_name: &'a str,
    pub identity: &'a Identity,
    pub language: &'a str,
    /// The `--override-exe` path relative to the install dir (Heroic `targetExe`), if any.
    pub exe_override: Option<&'a str>,
    /// User arguments: Heroic launch option + `launcherArgs` (cli.py `extra`).
    pub extra_args: &'a [String],
    pub offline: bool,
    /// Path of the ownership-token file (when `requires_ot` and online).
    pub ovt_path: Option<&'a Path>,
}

/// The game's own arguments, in legendary's order (cli.py:698-701):
/// game executable, `game_parameters` (installed.json then metadata), `user_parameters`, `egl_parameters`.
/// The executable itself is returned separately as an absolute path.
pub fn game_args(install: &InstalledGame, meta: &GameMeta, i: &ArgInputs<'_>) -> (PathBuf, Vec<PlanArg>) {
    // core.py:848-857 executable (override or installed.json), `\` -> `/`, `lstrip('/')` for the default.
    let rel = match i.exe_override {
        Some(o) => o.replace('\\', "/"),
        None => install.executable.replace('\\', "/").trim_start_matches('/').to_string(),
    };
    let exe = install.install_path.join(&rel);
    let mut args: Vec<PlanArg> = Vec::new();

    // core.py:869-882 game_parameters: installed.json launch_parameters, then metadata AdditionalCommandLine
    for p in shlex_split_non_posix(&install.launch_parameters) {
        args.push(PlanArg::Lit(p));
    }
    if let Some(extra) = &meta.additional_command_line {
        for p in shlex_split_non_posix(extra.trim()) {
            args.push(PlanArg::Lit(p));
        }
    }
    // core.py:927-931 user_parameters
    for a in i.extra_args {
        args.push(PlanArg::Lit(a.clone()));
    }

    // core.py:897-902 egl_parameters. With `offline`, the token is empty (core.py:883 `game_token = ''`).
    args.push(PlanArg::lit("-AUTH_LOGIN=unused"));
    if i.offline {
        args.push(PlanArg::lit("-AUTH_PASSWORD="));
    } else {
        args.push(PlanArg::ExchangeCode { prefix: "-AUTH_PASSWORD=".into() });
    }
    args.push(PlanArg::lit("-AUTH_TYPE=exchangecode"));
    args.push(PlanArg::Lit(format!("-epicapp={}", i.app_name)));
    args.push(PlanArg::lit("-epicenv=Prod"));
    // core.py:904-911 ownership token file
    if let Some(p) = i.ovt_path {
        args.push(PlanArg::Lit(format!("-epicovt={}", p.display())));
    }
    // core.py:917-924
    args.push(PlanArg::lit("-EpicPortal"));
    args.push(PlanArg::Lit(format!("-epicusername={}", i.identity.display_name)));
    args.push(PlanArg::Lit(format!("-epicuserid={}", i.identity.account_id)));
    args.push(PlanArg::Lit(format!("-epiclocale={}", i.language)));
    args.push(PlanArg::Lit(format!("-epicsandboxid={}", meta.namespace)));
    // core.py:926-929
    if let Some(d) = &meta.deployment_id {
        args.push(PlanArg::Lit(format!("-epicdeploymentid={d}")));
    }
    (exe, args)
}

/// Where legendary writes the ownership token (`lgd.get_tmp_path()`/`<namespace><catalog_item_id>.ovt`, core.py:907).
pub fn ovt_path(legendary_dir: &Path, meta: &GameMeta) -> PathBuf {
    legendary_dir.join("tmp").join(format!("{}{}.ovt", meta.namespace, meta.catalog_item_id))
}

pub fn split_args(s: &str) -> Vec<String> {
    shlex_split(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_posix_split_keeps_quotes() {
        assert_eq!(shlex_split_non_posix(r#"-a "b c" -d"#), ["-a", "\"b c\"", "-d"]);
        assert_eq!(shlex_split_non_posix("  "), Vec::<String>::new());
        assert_eq!(shlex_split_non_posix("x=\"a b\""), ["x=\"a", "b\""]);
    }
}
