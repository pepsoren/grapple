// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from legendary (GPLv3); see NOTICE.md.
//! Epic games without Epic or Heroic running: sign in ourselves (see `epic_auth`), then run the game exactly as
//! legendary + Heroic would: through umu-run (or the GE-Proton script) with the game's own Heroic settings.
//!
//! On Windows the game's executable runs directly (no Proton). Games the Epic Games Launcher installed are found
//! through its manifests ([`grapple_core::egl`]); they start the same way when there is a legendary/Heroic login
//! (`user.json`) to get an exchange code from. Without one, grapple cannot sign the game in, so it asks the Epic
//! Games Launcher to start it (`com.epicgames.launcher://apps/...?action=launch&silent=true`): the launcher
//! starts (into its tray when it was not running) and runs the game signed in.

use crate::epic_auth::{EpicAuth, Identity};
use crate::error::{LaunchError, Result};
use crate::heroic::{HeroicSettings, RuntimeInput, cached_umu_id, windows_runtime};
use crate::legendary::{ArgInputs, GameMeta, InstalledGame, game_args, ovt_path};
use crate::plan::{LaunchPlan, PlanArg};
use crate::proc::Matcher;
use crate::settings::LaunchSettings;
use crate::spawn::spawn_plan;
use crate::strategy::{BoxFuture, LaunchStrategy, STEP_STARTING, StartCtx, Started};
use grapple_core::Game;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn game_id_of(app_name: &str) -> grapple_core::GameId {
    grapple_core::GameId::new(grapple_core::Store::Epic, app_name)
}

pub const STEP_SIGN_IN: &str = "Epic sign-in token";
pub const STEP_PROTON: &str = "Proton ready";

/// Who the plan is for. A real launch uses the signed-in account; dry runs use placeholders so no login file is read.
pub enum PlanIdentity {
    Real(Identity),
    Placeholder,
}

impl PlanIdentity {
    fn get(&self) -> Identity {
        match self {
            PlanIdentity::Real(i) => i.clone(),
            PlanIdentity::Placeholder => Identity { account_id: "<account-id>".into(), display_name: "<display-name>".into() },
        }
    }
}

pub struct EpicStrategy {
    pub settings: LaunchSettings,
    pub path_var: String,
}

impl EpicStrategy {
    pub fn new(settings: LaunchSettings) -> Self {
        EpicStrategy { settings, path_var: crate::env::child_path() }
    }

    /// The game's install: legendary's (Heroic's) first, then the Epic Games Launcher's (`Some` third value).
    pub fn install_of(&self, app_name: &str) -> Result<(InstalledGame, GameMeta, Option<grapple_core::egl::EglGame>)> {
        let s = &self.settings;
        match InstalledGame::load(&s.legendary_dir, app_name) {
            Ok(install) => Ok((install, GameMeta::load(&s.legendary_dir, app_name)?, None)),
            Err(e @ (LaunchError::NotInstalled(_) | LaunchError::Read { .. })) => {
                let Some(g) = s.egl_dir.as_deref().and_then(|d| grapple_core::egl::find(d, app_name)) else { return Err(e) };
                let meta = GameMeta::load(&s.legendary_dir, app_name).unwrap_or_else(|_| GameMeta::from_egl(&g));
                Ok((InstalledGame::from_egl(&g), meta, Some(g)))
            }
            Err(e) => Err(e),
        }
    }

    /// Build the command a launch would run, without any network or secrets. `offline`: skip the token.
    pub fn plan(&self, app_name: &str, identity: &PlanIdentity, offline: bool, ovt: Option<&Path>) -> Result<LaunchPlan> {
        let s = &self.settings;
        let (install, meta, _) = self.install_of(app_name)?;
        let mut heroic = HeroicSettings::load(&s.heroic_dir, app_name)?;
        let identity = identity.get();
        // Grapple's own per-game choice wins over Heroic's GameMode setting.
        let wrap = s.wrappers.get(&game_id_of(app_name)).copied();
        if let Some(w) = wrap {
            heroic.use_game_mode = w.gamemode;
        }
        // grapple's "Runs with" choice (install sheet) wins over Heroic's Proton; the prefix stays Heroic's.
        if let Some(bin) = s.proton_per_game.get(&game_id_of(app_name)) {
            let name = bin.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            heroic.wine_version = crate::heroic::WineVersion { bin: bin.to_string_lossy().into_owned(), name, kind: "proton".into() };
        }
        let want_mangohud = wrap.is_some_and(|w| w.mangohud);
        let mangohud = if want_mangohud { crate::paths::which_in("mangohud", &self.path_var) } else { None };

        let language = if heroic.language.is_empty() { "en" } else { heroic.language.as_str() };
        let extra = heroic.extra_args();
        let exe_override = (!heroic.target_exe.is_empty()).then_some(heroic.target_exe.as_str());
        let (exe, mut args) = game_args(
            &install,
            &meta,
            &ArgInputs { app_name, identity: &identity, language, exe_override, extra_args: &extra, offline, ovt_path: ovt },
        );

        let folder = install.install_path.file_name().and_then(|n| n.to_str()).unwrap_or(app_name).to_string();
        let game_id = cached_umu_id(&s.heroic_dir, &format!("legendary_{app_name}")).unwrap_or_else(|| "umu-0".into());
        let mut extra_env = BTreeMap::new();
        extra_env.insert("GRAPPLE_GAME".to_string(), format!("epic:{app_name}"));
        let gamemode = crate::gamemode::choose(heroic.use_game_mode, &self.path_var);
        let register = match &gamemode {
            crate::gamemode::Via::Register(bin) => Some(bin.clone()),
            _ => None,
        };

        // core.py:811-816 / cli.py:694-697: launch_command (wrappers + runner) then the executable.
        let (mut argv, env, mut notes) = if needs_wine(&install) {
            let rt = windows_runtime(&RuntimeInput {
                settings: &heroic,
                heroic_dir: &s.heroic_dir,
                umu_override: s.umu_run.as_deref(),
                path_var: &self.path_var,
                steam_client_path: &s.steam_client_path,
                install_path: Some(&install.install_path),
                steam_game_id: format!("heroic-{folder}"),
                umu_game_id: game_id,
                umu_store: Some("egs"),
                extra_env,
                gamemode,
                umu_dir: &s.umu_dir,
                mangohud: want_mangohud,
                mangohud_bin: mangohud,
                pad_mode: s.pad_mode_for(&game_id_of(app_name)),
            })?;
            (rt.argv_prefix, rt.env, rt.notes)
        } else {
            // core.py:838-841: not a Windows build (or on Windows) -> no wine
            let mut a = Vec::new();
            let mut notes = vec![if cfg!(windows) { "Windows game, started directly" } else { "native game, no Wine" }.to_string()];
            if let crate::gamemode::Via::Wrap(g) = gamemode {
                a.push(g.to_string_lossy().into_owned());
            }
            if want_mangohud {
                match &mangohud {
                    Some(m) => a.push(m.to_string_lossy().into_owned()),
                    None => notes.push("MangoHud was requested but is not installed; starting without it".into()),
                }
            }
            (a, extra_env, notes)
        };
        if offline {
            notes.push("offline: no sign-in token".into());
        }
        argv.push(exe.to_string_lossy().into_owned());
        let program = argv.remove(0);
        let mut all: Vec<PlanArg> = argv.into_iter().map(PlanArg::Lit).collect();
        all.append(&mut args);
        // cli.py:151 `cwd=params.working_directory` (core.py:859 `os.path.split(exe_path)[0]`)
        let cwd = exe.parent().map(Path::to_path_buf);
        Ok(LaunchPlan { program, args: all, env, cwd, notes, gamemode: register })
    }

    /// Fetch the ownership token file for games that need one (core.py:904-911).
    async fn write_ovt(&self, auth: &EpicAuth, identity: &Identity, access: &crate::plan::Secret, meta: &GameMeta) -> Result<PathBuf> {
        let bytes = auth.ownership_token(identity, access, &meta.namespace, &meta.catalog_item_id).await?;
        let path = ovt_path(&self.settings.legendary_dir, meta);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}

/// A Windows build on a system that is not Windows runs through Wine/Proton.
fn needs_wine(install: &InstalledGame) -> bool {
    install.platform.starts_with("Win") && !cfg!(windows)
}

/// Start an Epic Games Launcher game through the launcher itself (no legendary login to sign it in with).
// untested on Windows: the URI form is the one the launcher's own desktop shortcuts use.
fn start_through_launcher(game: &Game, egl: &grapple_core::egl::EglGame) -> Result<Started> {
    let uri = egl.launch_uri();
    // explorer.exe hands a link to its registered handler (here the Epic Games Launcher) and exits.
    crate::spawn::spawn_outside("explorer.exe", &[uri])?;
    Ok(Started {
        // Not grapple's child: the game is recognised by its install folder (see `winproc`).
        matcher: Matcher::new(game.id.clone()),
        child_exit: None,
        // The launcher may have to start, sign in and check the files first.
        appear_timeout: Duration::from_secs(180),
        min_alive: Duration::ZERO,
        log: None,
        final_step: STEP_STARTING,
        umu_log: false,
        gamemode: None,
    })
}

impl LaunchStrategy for EpicStrategy {
    fn name(&self) -> &'static str {
        "epic"
    }

    fn start<'a>(&'a self, game: &'a Game, ctx: &'a StartCtx) -> BoxFuture<'a, Result<Started>> {
        Box::pin(async move {
            let s = &self.settings;
            let app = game.id.store_id().to_string();
            let (install, meta, egl) = self.install_of(&app)?;
            let heroic = HeroicSettings::load(&s.heroic_dir, &app)?;
            let user_json = s.legendary_dir.join("user.json");
            if let Some(g) = egl.as_ref().filter(|g| g.launch_executable.is_empty() || !user_json.is_file()) {
                tracing::info!("{app}: no Epic login for grapple; starting it through the Epic Games Launcher");
                ctx.step(STEP_STARTING, false).await;
                return start_through_launcher(game, g);
            }
            let auth = EpicAuth::new(s.epic_api.clone(), user_json)?;

            ctx.step(STEP_SIGN_IN, false).await;
            let mut offline = heroic.offline_mode;
            let mut token = None;
            if !offline {
                match auth.game_token().await {
                    Ok(t) => token = Some(t),
                    // core.py:883 offline path; we fall back automatically when Epic is unreachable.
                    Err(LaunchError::Network(why)) if install.can_run_offline => {
                        tracing::info!("Epic unreachable ({why}); starting {app} offline");
                        offline = true;
                    }
                    Err(e) => return Err(e),
                }
            } else if !install.can_run_offline {
                // core.py:884-885 only warns
                tracing::warn!("{app} is not approved for offline use and may not work");
            }
            let identity = match &token {
                Some(t) => t.identity.clone(),
                None => auth.identity()?,
            };
            let ovt = match (&token, install.requires_ot && !offline) {
                (Some(t), true) => Some(self.write_ovt(&auth, &t.identity, &t.access, &meta).await?),
                _ => None,
            };
            ctx.step(STEP_SIGN_IN, true).await;

            // Windows runs the game directly: no Proton step, no prefix.
            let proton_step = !cfg!(windows);
            if proton_step {
                ctx.step(STEP_PROTON, false).await;
            }
            let plan = self.plan(&app, &PlanIdentity::Real(identity), offline, ovt.as_deref())?;
            let prefix = heroic.wine_prefix.replace('\'', "");
            if cfg!(windows) && s.eos_overlay_games.contains(&game.id) {
                epic_overlay_windows(&app, s.eos_overlay_dir.as_deref());
            }
            if proton_step && !prefix.is_empty() {
                // Proton wants its prefix directory to exist; first runs create the contents.
                std::fs::create_dir_all(&prefix)?;
                // Epic's in-game overlay (store checkouts), on or off as the game's setting says.
                let ours = s.eos_overlay_dir.as_deref();
                let want = ours.filter(|d| s.eos_overlay_games.contains(&game.id) && d.join(crate::eos::MARKER).is_file());
                match crate::eos::sync(Path::new(&prefix), want, ours) {
                    Ok(crate::eos::Synced::Unchanged) => {}
                    Ok(done) => tracing::info!("{app}: Epic overlay {done:?}"),
                    Err(e) => tracing::warn!("{app}: could not set Epic's overlay: {e}"),
                }
            }
            if proton_step {
                ctx.step(STEP_PROTON, true).await;
            }

            ctx.step(STEP_STARTING, false).await;
            let log = ctx.log_dir.join(format!("epic-{app}.log"));
            let spawned = spawn_plan(&plan, token.as_ref().map(|t| &t.code), Some(&log), &ctx.proc)?;
            let windows = needs_wine(&install);
            Ok(Started {
                matcher: Matcher { game: game.id.clone(), root: Some((spawned.pid, spawned.starttime)) },
                child_exit: Some(spawned.exit),
                appear_timeout: Duration::from_secs(30),
                min_alive: Duration::from_millis(2500),
                log: Some(log),
                final_step: STEP_STARTING,
                umu_log: plan.notes.iter().any(|n| n == crate::heroic::NOTE_UMU),
                gamemode: plan.gamemode.clone().map(|bin| crate::gamemode::Request { bin, windows }),
            })
        })
    }
}

/// Windows: make sure Epic's overlay is set up for the user (see [`crate::eos::sync_windows`]).
#[cfg(windows)]
fn epic_overlay_windows(app: &str, ours: Option<&Path>) {
    let want = ours.filter(|d| d.join(crate::eos::MARKER).is_file());
    match crate::eos::sync_windows(want) {
        Ok(crate::eos::Synced::Unchanged) => {}
        Ok(done) => tracing::info!("{app}: Epic overlay {done:?}"),
        Err(e) => tracing::warn!("{app}: could not set Epic's overlay: {e}"),
    }
}

#[cfg(not(windows))]
fn epic_overlay_windows(_app: &str, _ours: Option<&Path>) {}
