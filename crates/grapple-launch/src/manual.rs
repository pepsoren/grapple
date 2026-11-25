//! Manual games: a native binary, an AppImage, a Windows `.exe` through Proton, or any command.
//! On Windows a `.exe` (or `.bat`/`.cmd`) runs directly; AppImages and Proton do not exist there, and shortcuts
//! (`.lnk`, `.url`) are refused with a clear message (their target cannot be tracked as the game).

use crate::error::{LaunchError, Result};
use crate::heroic::{HeroicSettings, RuntimeInput, WineVersion, windows_runtime};
use crate::paths;
use crate::plan::{LaunchPlan, PlanArg};
use crate::proc::Matcher;
use crate::settings::LaunchSettings;
use crate::spawn::spawn_plan;
use crate::strategy::{BoxFuture, LaunchStrategy, STEP_STARTING, StartCtx, Started};
use grapple_core::Game;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualKind {
    /// Decide from the file: `.exe`/`.bat`/`.msi` -> Windows, `.AppImage` -> AppImage, a file path -> native,
    /// anything else is a command looked up on `PATH`.
    #[default]
    Auto,
    Native,
    AppImage,
    /// Windows program, run through Proton (umu-run when available).
    Windows,
    /// Arbitrary command (looked up on `PATH`).
    Command,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManualEntry {
    /// Path to the program, or a command name.
    pub exe: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Working directory (default: the program's folder).
    pub cwd: Option<PathBuf>,
    pub kind: ManualKind,
    /// Windows only: Proton to use (a `proton` script) and the prefix folder.
    pub proton: Option<PathBuf>,
    pub prefix: Option<PathBuf>,
    pub gamemode: bool,
    /// MangoHud overlay (`mangohud`, plus `--dlsym` for Windows programs like Heroic does).
    pub mangohud: bool,
}

impl ManualEntry {
    pub fn effective_kind(&self) -> ManualKind {
        match self.kind {
            ManualKind::Auto => {
                let lower = self.exe.to_lowercase();
                if [".exe", ".bat", ".msi", ".cmd"].iter().any(|e| lower.ends_with(e)) {
                    ManualKind::Windows
                } else if lower.ends_with(".appimage") {
                    ManualKind::AppImage
                } else if self.exe.contains('/') || (cfg!(windows) && self.exe.contains('\\')) {
                    ManualKind::Native
                } else {
                    ManualKind::Command
                }
            }
            k => k,
        }
    }
}

pub struct ManualStrategy {
    pub settings: LaunchSettings,
    pub path_var: String,
}

impl ManualStrategy {
    pub fn new(settings: LaunchSettings) -> Self {
        ManualStrategy { settings, path_var: crate::env::child_path() }
    }

    pub fn plan(&self, game: &Game) -> Result<LaunchPlan> {
        let s = &self.settings;
        let entry = s
            .manual
            .get(&game.id)
            .ok_or_else(|| LaunchError::Config(format!("no launch details for {}; edit the game and set its program", game.name)))?;
        if entry.exe.trim().is_empty() {
            return Err(LaunchError::Config(format!("{} has no program set", game.name)));
        }
        let kind = entry.effective_kind();
        if cfg!(windows) {
            windows_supported(entry, kind)?;
        }
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("GRAPPLE_GAME".into(), game.id.0.clone());
        let mut notes = Vec::new();
        // Grapple's per-game switches (if any) replace the entry's own flags.
        let (want_gamemode, want_mangohud) = match s.wrappers.get(&game.id) {
            Some(w) => (w.gamemode, w.mangohud),
            None => (entry.gamemode, entry.mangohud),
        };
        let gamemode = crate::gamemode::choose(want_gamemode, &self.path_var);
        let register = match &gamemode {
            crate::gamemode::Via::Register(bin) => Some(bin.clone()),
            _ => None,
        };
        let mangohud = if want_mangohud { paths::which_in("mangohud", &self.path_var) } else { None };
        let mut argv: Vec<String> = Vec::new();
        let exe_path = PathBuf::from(&entry.exe);
        let default_cwd = exe_path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);

        match kind {
            // Windows itself: the program runs as it is.
            ManualKind::Windows if cfg!(windows) => {
                argv.push(entry.exe.clone());
                notes.push("Windows program, started directly".into());
            }
            ManualKind::Windows => {
                let proton = entry
                    .proton
                    .clone()
                    .or_else(|| s.default_proton.clone())
                    .or_else(|| heroic_default_proton(&s.heroic_dir))
                    .ok_or_else(|| LaunchError::Config("no Proton is set up; choose one for this game".into()))?;
                let prefix = entry
                    .prefix
                    .clone()
                    .unwrap_or_else(|| paths::data_home().join("grapple").join("prefixes").join(game.id.store_id()));
                let settings = HeroicSettings {
                    wine_version: WineVersion {
                        bin: proton.to_string_lossy().into_owned(),
                        name: String::new(),
                        kind: "proton".into(),
                    },
                    wine_prefix: prefix.to_string_lossy().into_owned(),
                    use_game_mode: want_gamemode,
                    ..HeroicSettings::default()
                };
                let rt = windows_runtime(&RuntimeInput {
                    settings: &settings,
                    heroic_dir: &s.heroic_dir,
                    umu_override: s.umu_run.as_deref(),
                    path_var: &self.path_var,
                    steam_client_path: &s.steam_client_path,
                    install_path: exe_path.parent(),
                    steam_game_id: format!("grapple-{}", game.id.store_id()),
                    umu_game_id: "umu-0".into(),
                    umu_store: None,
                    extra_env: BTreeMap::new(),
                    gamemode,
                    umu_dir: &s.umu_dir,
                    mangohud: want_mangohud,
                    mangohud_bin: mangohud,
                    pad_mode: s.pad_mode_for(&game.id),
                })?;
                argv.extend(rt.argv_prefix);
                for (k, v) in rt.env {
                    env.insert(k, v);
                }
                notes.extend(rt.notes);
                argv.push(entry.exe.clone());
            }
            ManualKind::Native | ManualKind::AppImage | ManualKind::Auto => {
                wrap_native(&mut argv, gamemode, mangohud, want_mangohud, &mut notes);
                argv.push(entry.exe.clone());
                if kind == ManualKind::AppImage {
                    notes.push("AppImage (made executable if needed)".into());
                }
            }
            ManualKind::Command => {
                wrap_native(&mut argv, gamemode, mangohud, want_mangohud, &mut notes);
                argv.push(entry.exe.clone());
            }
        }
        argv.extend(entry.args.iter().cloned());
        for (k, v) in &entry.env {
            env.insert(k.clone(), v.clone());
        }
        let program = argv.remove(0);
        Ok(LaunchPlan {
            program,
            args: argv.into_iter().map(PlanArg::Lit).collect(),
            env,
            cwd: entry.cwd.clone().or(default_cwd),
            notes,
            gamemode: register,
        })
    }
}

/// What Windows cannot run: AppImages, and shortcuts, whose program grapple would not see start.
fn windows_supported(entry: &ManualEntry, kind: ManualKind) -> Result<()> {
    let lower = entry.exe.to_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".url") {
        return Err(LaunchError::Config("shortcuts (.lnk, .url) cannot be added yet; choose the game's .exe instead".into()));
    }
    if kind == ManualKind::AppImage {
        return Err(LaunchError::Config("AppImages run on Linux only".into()));
    }
    Ok(())
}

/// `[gamemoderun] mangohud <program>` for programs that run without Wine (`gamemoderun` only where there is no
/// `gamemoded` to register the game with, see [`crate::gamemode`]).
fn wrap_native(argv: &mut Vec<String>, gamemode: crate::gamemode::Via, mangohud: Option<PathBuf>, want_mangohud: bool, notes: &mut Vec<String>) {
    if let crate::gamemode::Via::Wrap(g) = gamemode {
        argv.push(g.to_string_lossy().into_owned());
    }
    match mangohud {
        Some(m) => argv.push(m.to_string_lossy().into_owned()),
        None if want_mangohud => notes.push("MangoHud was requested but is not installed; starting without it".into()),
        None => {}
    }
}

/// Heroic's configured default Proton (`config.json` -> defaultSettings.wineVersion), if it is a Proton.
fn heroic_default_proton(heroic_dir: &Path) -> Option<PathBuf> {
    let s = HeroicSettings::load(heroic_dir, "__none__").ok()?;
    (s.wine_version.kind == "proton" && Path::new(&s.wine_version.bin).exists()).then(|| PathBuf::from(s.wine_version.bin))
}

impl LaunchStrategy for ManualStrategy {
    fn name(&self) -> &'static str {
        "manual"
    }

    fn start<'a>(&'a self, game: &'a Game, ctx: &'a StartCtx) -> BoxFuture<'a, Result<Started>> {
        Box::pin(async move {
            let entry = self.settings.manual.get(&game.id).cloned();
            if let Some(e) = &entry {
                if e.effective_kind() == ManualKind::AppImage {
                    ensure_executable(Path::new(&e.exe))?;
                }
                if e.effective_kind() == ManualKind::Windows && !cfg!(windows) {
                    ctx.step(crate::epic::STEP_PROTON, false).await;
                }
            }
            let plan = self.plan(game)?;
            if let Some(e) = &entry {
                if e.effective_kind() == ManualKind::Windows && !cfg!(windows) {
                    if let Some(prefix) = plan.env.get("WINEPREFIX") {
                        std::fs::create_dir_all(prefix)?;
                    }
                    ctx.step(crate::epic::STEP_PROTON, true).await;
                }
            }
            ctx.step(STEP_STARTING, false).await;
            let log = ctx.log_dir.join(format!("manual-{}.log", game.id.store_id()));
            let spawned = spawn_plan(&plan, None, Some(&log), &ctx.proc)?;
            let windows = !cfg!(windows) && entry.as_ref().is_some_and(|e| e.effective_kind() == ManualKind::Windows);
            Ok(Started {
                matcher: Matcher { game: game.id.clone(), root: Some((spawned.pid, spawned.starttime)) },
                child_exit: Some(spawned.exit),
                appear_timeout: Duration::from_secs(30),
                min_alive: Duration::from_millis(1500),
                log: Some(log),
                final_step: STEP_STARTING,
                umu_log: plan.notes.iter().any(|n| n == crate::heroic::NOTE_UMU),
                gamemode: plan.gamemode.clone().map(|bin| crate::gamemode::Request { bin, windows }),
            })
        })
    }
}

#[cfg(windows)]
fn ensure_executable(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn ensure_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(p).map_err(|_| LaunchError::Missing { what: "the AppImage", detail: p.display().to_string() })?;
    let mode = meta.permissions().mode();
    if mode & 0o100 == 0 {
        let mut perms = meta.permissions();
        perms.set_mode(mode | 0o100);
        std::fs::set_permissions(p, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_refuses_what_it_cannot_run() {
        let e = |exe: &str| ManualEntry { exe: exe.into(), ..Default::default() };
        assert!(windows_supported(&e("C:\\Games\\Celeste\\Celeste.exe"), ManualKind::Windows).is_ok());
        assert!(windows_supported(&e("C:\\Users\\j\\Desktop\\Game.lnk"), ManualKind::Native).is_err());
        assert!(windows_supported(&e("C:\\Users\\j\\Desktop\\Game.URL"), ManualKind::Native).is_err());
        assert!(windows_supported(&e("game.AppImage"), ManualKind::AppImage).is_err());
    }
}
