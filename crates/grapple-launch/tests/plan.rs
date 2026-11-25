//! Command-line plans from fixture configs (nothing is started, nothing is read from the real home).

// Linux (and other Unix) only: Proton/umu launch plans; Windows runs games directly.
#![cfg(unix)]

mod common;

use common::*;
use grapple_core::{Art, Game, GameId, Store};
use grapple_launch::epic::{EpicStrategy, PlanIdentity};
use grapple_launch::manual::{ManualEntry, ManualKind, ManualStrategy};
use grapple_launch::plan::{PlanArg, Secret};
use grapple_launch::steam::{SteamLaunchVia, SteamSettings, SteamStrategy};
use grapple_launch::LaunchSettings;
use std::path::Path;

fn game(store: Store, id: &str) -> Game {
    Game {
        id: GameId::new(store, id),
        store,
        name: id.into(),
        sort_name: id.into(),
        installed: true,
        install_dir: None,
        size_bytes: None,
        art: Art::default(),
        store_playtime_min: 0,
        last_played: None,
        status: None,
    }
}

fn settings(dir: &Path) -> LaunchSettings {
    LaunchSettings {
        heroic_dir: dir.join("heroic"),
        legendary_dir: dir.join("legendary"),
        log_dir: dir.join("logs"),
        umu_dir: dir.join("umu"),
        ..LaunchSettings::default()
    }
}

/// A Heroic + legendary tree like Josh's, in a temp dir. `extra_game_cfg` is merged into the game's Heroic config.
fn epic_tree(dir: &Path, game_cfg: &str, installed_extra: &str) {
    write_exec(&dir.join("proton/GE/proton"), "#!/bin/sh\n");
    write(&dir.join("heroic/config.json"), r#"{"defaultSettings":{"language":"en"}}"#);
    write(
        &dir.join("heroic/GamesConfig/Sugar.json"),
        &format!(
            r#"{{"Sugar":{{"wineVersion":{{"bin":"{}","name":"GE","type":"proton"}},"winePrefix":"{}"{game_cfg}}},"version":"v0"}}"#,
            dir.join("proton/GE/proton").display(),
            dir.join("pfx").display()
        ),
    );
    write(&dir.join("heroic/store_cache/umu.json"), r#"{"legendary_Sugar":"umu-252950"}"#);
    write(
        &dir.join("legendary/installed.json"),
        &format!(
            r#"{{"Sugar":{{"app_name":"Sugar","title":"Rocket League","install_path":"{}","executable":"Binaries/Win64/Launcher.exe",
"launch_parameters":"","can_run_offline":true,"requires_ot":false,"platform":"Windows"{installed_extra}}}}}"#,
            dir.join("rl").display()
        ),
    );
    write(&dir.join("legendary/metadata/Sugar.json"), r#"{"metadata":{"namespace":"ns77","id":"cat88"}}"#);
}

#[test]
fn epic_plan_through_umu_uses_heroics_runtime_and_gamemode() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), r#","useGameMode":true"#, "");
    write_exec(&dir.path().join("umu-run"), "#!/bin/sh\n");
    write_exec(&dir.path().join("bin/gamemoderun"), "#!/bin/sh\n");
    let mut s = settings(dir.path());
    s.umu_run = Some(dir.path().join("umu-run"));
    let mut strat = EpicStrategy::new(s);
    strat.path_var = dir.path().join("bin").to_string_lossy().into_owned();

    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    assert_eq!(plan.program, dir.path().join("bin/gamemoderun").to_str().unwrap());
    let args = plan.resolve_args(Some(&Secret::new("CODE")));
    assert_eq!(args[0], dir.path().join("umu-run").to_str().unwrap());
    assert_eq!(args[1], dir.path().join("rl/Binaries/Win64/Launcher.exe").to_str().unwrap());
    assert_eq!(
        &args[2..],
        [
            "-AUTH_LOGIN=unused",
            "-AUTH_PASSWORD=CODE",
            "-AUTH_TYPE=exchangecode",
            "-epicapp=Sugar",
            "-epicenv=Prod",
            "-EpicPortal",
            "-epicusername=<display-name>",
            "-epicuserid=<account-id>",
            "-epiclocale=en",
            "-epicsandboxid=ns77",
        ]
    );
    assert_eq!(plan.cwd.as_deref(), Some(dir.path().join("rl/Binaries/Win64").as_path()));
    assert_eq!(plan.env["GAMEID"], "umu-252950", "umu id from Heroic's cache");
    assert_eq!(plan.env["STORE"], "egs");
    assert_eq!(plan.env["PROTONPATH"], dir.path().join("proton/GE").to_str().unwrap());
    // the printed form never contains the real code
    let shown = plan.display_command();
    assert!(shown.contains("<exchange-code>") && !shown.contains("CODE"));
}

#[test]
fn epic_plan_without_umu_runs_the_proton_script_with_waitforexitandrun() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), r#","disableUMU":true,"enviromentOptions":[{"key":"A","value":"\"x y\""}],"launcherArgs":"-windowed -foo=\"a b\"""#, "");
    let mut strat = EpicStrategy::new(settings(dir.path()));
    strat.path_var = String::new();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    let proton = dir.path().join("proton/GE/proton");
    assert_eq!(plan.program, proton.to_str().unwrap());
    let args = plan.resolve_args(None);
    assert_eq!(args[0], "waitforexitandrun");
    assert!(args.contains(&"-windowed".to_string()) && args.contains(&"-foo=a b".to_string()));
    // user parameters come before the egl parameters (cli.py: game, user, egl)
    let pos = |needle: &str| args.iter().position(|a| a == needle).unwrap();
    assert!(pos("-windowed") < pos("-AUTH_LOGIN=unused"));
    assert_eq!(plan.env["A"], "x y", "quotes around Heroic env values are removed");
    assert_eq!(plan.env["STEAM_COMPAT_DATA_PATH"], dir.path().join("pfx").to_str().unwrap());
}

#[test]
fn epic_plan_offline_has_an_empty_password_and_ownership_token_is_passed() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), "", "");
    let mut strat = EpicStrategy::new(settings(dir.path()));
    strat.path_var = String::new();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, true, None).unwrap();
    assert!(plan.args.contains(&PlanArg::Lit("-AUTH_PASSWORD=".into())));
    assert!(plan.notes.iter().any(|n| n.contains("offline")));
    let ovt = Path::new("/x/y.ovt");
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, Some(ovt)).unwrap();
    let args = plan.resolve_args(None);
    let pos = |needle: &str| args.iter().position(|a| a == needle).unwrap();
    assert_eq!(args[pos("-epicenv=Prod") + 1], "-epicovt=/x/y.ovt", "right after -epicenv, like core.py");
}

#[test]
fn epic_plan_errors_are_clear() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), "", "");
    let strat = EpicStrategy::new(settings(dir.path()));
    let err = strat.plan("Nope", &PlanIdentity::Placeholder, false, None).unwrap_err();
    assert!(err.to_string().contains("Nope is not installed"));
    // a missing Proton is reported by name
    std::fs::remove_file(dir.path().join("proton/GE/proton")).unwrap();
    let err = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap_err();
    assert!(err.to_string().contains("proton/GE/proton"), "{err}");
}

#[test]
fn steam_plans_follow_the_flags_in_one_place() {
    let mut s = SteamSettings::default();
    let strat = SteamStrategy::new(s.clone());
    let g = game(Store::Steam, "730");
    assert_eq!(strat.plan(&g, true).display_command(), "steam -applaunch 730");
    let cold = strat.plan(&g, false);
    assert_eq!(cold.display_command(), "steam -applaunch 730", "cold: Steam starts alone first, then the same hand-over");
    assert!(cold.notes[0].contains("steam -silent -nofriendsui") && cold.notes[0].contains("steam.pipe"), "{:?}", cold.notes);
    s.flags.launch_via = SteamLaunchVia::RunGameIdUrl;
    let strat = SteamStrategy::new(s);
    assert_eq!(strat.plan(&g, true).display_command(), "steam steam://rungameid/730");
}

#[test]
fn manual_plans_for_native_appimage_command_and_windows() {
    let dir = tempfile::tempdir().unwrap();
    write_exec(&dir.path().join("proton/proton"), "#!/bin/sh\n");
    let mut s = settings(dir.path());
    s.default_proton = Some(dir.path().join("proton/proton"));
    s.umu_run = None;
    let mk = |exe: &str, kind: ManualKind| ManualEntry { exe: exe.into(), args: vec!["--fast".into()], kind, ..Default::default() };
    s.manual.insert(GameId::new(Store::Manual, "n"), mk("/opt/g/game", ManualKind::Auto));
    s.manual.insert(GameId::new(Store::Manual, "a"), mk("/opt/g/Thing.AppImage", ManualKind::Auto));
    s.manual.insert(GameId::new(Store::Manual, "c"), ManualEntry { env: [("K".to_string(), "V".to_string())].into(), ..mk("mygame", ManualKind::Auto) });
    s.manual.insert(GameId::new(Store::Manual, "w"), ManualEntry { prefix: Some(dir.path().join("pfx")), ..mk("/games/x/Game.exe", ManualKind::Auto) });
    let mut strat = ManualStrategy::new(s);
    strat.path_var = String::new();

    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert_eq!((p.program.as_str(), p.resolve_args(None)), ("/opt/g/game", vec!["--fast".to_string()]));
    assert_eq!(p.cwd.as_deref(), Some(Path::new("/opt/g")));
    assert_eq!(p.env["GRAPPLE_GAME"], "manual:n");
    let p = strat.plan(&game(Store::Manual, "a")).unwrap();
    assert_eq!(p.program, "/opt/g/Thing.AppImage");
    let p = strat.plan(&game(Store::Manual, "c")).unwrap();
    assert_eq!((p.program.as_str(), p.env["K"].as_str(), p.cwd), ("mygame", "V", None));
    let p = strat.plan(&game(Store::Manual, "w")).unwrap();
    assert_eq!(p.program, dir.path().join("proton/proton").to_str().unwrap());
    assert_eq!(p.resolve_args(None), ["waitforexitandrun", "/games/x/Game.exe", "--fast"]);
    assert_eq!(p.env["STEAM_COMPAT_DATA_PATH"], dir.path().join("pfx").to_str().unwrap());

    assert!(strat.plan(&game(Store::Manual, "missing")).is_err());
}

#[test]
fn close_steam_policy() {
    use grapple_launch::CloseSteam;
    let id = GameId::new(Store::Steam, "1");
    let mut s = LaunchSettings::default();
    assert!(s.should_close_steam(&id, true) && !s.should_close_steam(&id, false), "default: only a Steam we started");
    s.close_steam = CloseSteam::Always;
    assert!(s.should_close_steam(&id, false));
    s.close_steam = CloseSteam::Never;
    s.close_steam_per_game.insert(id.clone(), true);
    assert!(s.should_close_steam(&id, false), "per-game override wins");
    s.keep_steam_ready = true;
    assert!(!s.should_close_steam(&id, true), "keep ready means keep it");
}

#[test]
fn grapple_wrapper_choices_apply_to_epic_and_manual_games() {
    use grapple_launch::Wrappers;
    let dir = tempfile::tempdir().unwrap();
    // Heroic says GameMode on; Grapple's per-game switch says off, MangoHud on.
    epic_tree(dir.path(), r#","useGameMode":true"#, "");
    write_exec(&dir.path().join("umu-run"), "#!/bin/sh\n");
    write_exec(&dir.path().join("bin/gamemoderun"), "#!/bin/sh\n");
    write_exec(&dir.path().join("bin/mangohud"), "#!/bin/sh\n");
    let path_var = dir.path().join("bin").to_string_lossy().into_owned();
    let mut s = settings(dir.path());
    s.umu_run = Some(dir.path().join("umu-run"));

    // no entry: Heroic's own setting stays (GameMode on, no MangoHud)
    let mut strat = EpicStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    assert_eq!(plan.program, dir.path().join("bin/gamemoderun").to_str().unwrap());
    assert!(!plan.resolve_args(None).contains(&"--dlsym".to_string()));

    s.wrappers.insert(GameId::new(Store::Epic, "Sugar"), Wrappers { gamemode: false, mangohud: true });
    let mut strat = EpicStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    assert_eq!(plan.program, dir.path().join("bin/mangohud").to_str().unwrap(), "GameMode off, MangoHud first");
    let args = plan.resolve_args(None);
    assert_eq!(args[0], "--dlsym");
    assert_eq!(args[1], dir.path().join("umu-run").to_str().unwrap());

    s.wrappers.insert(GameId::new(Store::Epic, "Sugar"), Wrappers { gamemode: true, mangohud: true });
    let mut strat = EpicStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    assert_eq!(plan.program, dir.path().join("bin/gamemoderun").to_str().unwrap());
    assert_eq!(plan.resolve_args(None)[..2], [dir.path().join("bin/mangohud").to_str().unwrap(), "--dlsym"]);

    // manual native program: gamemoderun mangohud <exe>
    let id = GameId::new(Store::Manual, "n");
    s.manual.insert(id.clone(), ManualEntry { exe: "/opt/g/game".into(), kind: ManualKind::Native, ..Default::default() });
    let mut strat = ManualStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert_eq!((p.program.as_str(), p.resolve_args(None)), ("/opt/g/game", vec![]), "no switches, no wrappers");
    s.wrappers.insert(id, Wrappers { gamemode: true, mangohud: true });
    let mut strat = ManualStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert_eq!(p.program, dir.path().join("bin/gamemoderun").to_str().unwrap());
    assert_eq!(p.resolve_args(None), [dir.path().join("bin/mangohud").to_str().unwrap(), "/opt/g/game"]);

    // a requested wrapper that is not installed is a note, not a failure
    let mut strat = ManualStrategy::new(s);
    strat.path_var = String::new();
    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert_eq!(p.program, "/opt/g/game");
    assert!(p.notes.iter().any(|n| n.contains("MangoHud")));
}

#[test]
fn grapples_proton_choice_wins_over_heroics() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), "", "");
    write_exec(&dir.path().join("proton/GE-Proton10-25/proton"), "#!/bin/sh\n");
    let mut s = settings(dir.path());
    s.umu_run = Some(dir.path().join("no-umu"));
    s.proton_per_game.insert(GameId::new(Store::Epic, "Sugar"), dir.path().join("proton/GE-Proton10-25/proton"));
    let plan = EpicStrategy::new(s).plan("Sugar", &PlanIdentity::Placeholder, true, None).unwrap();
    assert_eq!(plan.program, dir.path().join("proton/GE-Proton10-25/proton").to_str().unwrap());
    assert_eq!(plan.env["PROTONPATH"], dir.path().join("proton/GE-Proton10-25").to_str().unwrap());
    // The prefix is still Heroic's.
    assert_eq!(plan.env["WINEPREFIX"], dir.path().join("pfx").to_str().unwrap());
}

#[test]
fn gamemode_registers_the_game_instead_of_preloading_into_umu() {
    let dir = tempfile::tempdir().unwrap();
    epic_tree(dir.path(), r#","useGameMode":true"#, "");
    write_exec(&dir.path().join("umu-run"), "#!/bin/sh\n");
    write_exec(&dir.path().join("bin/gamemoderun"), "#!/bin/sh\n");
    write_exec(&dir.path().join("bin/gamemoded"), "#!/bin/sh\n");
    let path_var = dir.path().join("bin").to_string_lossy().into_owned();
    let mut s = settings(dir.path());
    s.umu_run = Some(dir.path().join("umu-run"));

    // gamemoded is there: no LD_PRELOAD wrapper around umu, the game's pids are registered later
    let mut strat = EpicStrategy::new(s.clone());
    strat.path_var = path_var.clone();
    let plan = strat.plan("Sugar", &PlanIdentity::Placeholder, false, None).unwrap();
    assert_eq!(plan.program, dir.path().join("umu-run").to_str().unwrap());
    assert_eq!(plan.gamemode.as_deref(), Some(dir.path().join("bin/gamemoded").as_path()));
    assert!(!plan.resolve_args(None).iter().any(|a| a.contains("gamemoderun")));

    // the same for a native manual game
    let id = GameId::new(Store::Manual, "n");
    s.manual.insert(id, ManualEntry { exe: "/opt/g/game".into(), kind: ManualKind::Native, gamemode: true, ..Default::default() });
    let mut strat = ManualStrategy::new(s);
    strat.path_var = path_var;
    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert_eq!(p.program, "/opt/g/game");
    assert_eq!(p.gamemode.as_deref(), Some(dir.path().join("bin/gamemoded").as_path()));
}

#[test]
fn controller_mode_reaches_proton_games_and_the_users_env_wins() {
    use grapple_launch::pads::PadMode;
    let dir = tempfile::tempdir().unwrap();
    // Heroic's own environment option for the game names one of the variables.
    epic_tree(dir.path(), r#","enviromentOptions":[{"key":"PROTON_DISABLE_HIDRAW","value":"0x054C/0x0CE6"}]"#, "");
    write_exec(&dir.path().join("proton/proton"), "#!/bin/sh\n");
    let mut s = settings(dir.path());
    s.umu_run = Some(dir.path().join("no-umu"));
    s.default_proton = Some(dir.path().join("proton/proton"));
    s.pad_mode = PadMode::Xbox;
    let epic = |s: &LaunchSettings| EpicStrategy::new(s.clone()).plan("Sugar", &PlanIdentity::Placeholder, true, None).unwrap();
    let plan = epic(&s);
    assert_eq!(plan.env["PROTON_DISABLE_HIDRAW"], "0x054C/0x0CE6", "Heroic's value stays");
    assert_eq!(plan.env["PROTON_PREFER_SDL"], "1");
    // The game's own choice beats the default.
    s.pad_mode_per_game.insert(GameId::new(Store::Epic, "Sugar"), PadMode::AsIs);
    assert!(!epic(&s).env.contains_key("PROTON_PREFER_SDL"));

    // Manual Windows games get it too; the entry's own env wins; native games are left alone.
    let w = GameId::new(Store::Manual, "w");
    let n = GameId::new(Store::Manual, "n");
    s.manual.insert(w.clone(), ManualEntry {
        exe: "/games/x/Game.exe".into(),
        env: [("PROTON_PREFER_SDL".to_string(), "0".to_string())].into(),
        prefix: Some(dir.path().join("pfx")),
        ..Default::default()
    });
    s.manual.insert(n.clone(), ManualEntry { exe: "/opt/g/game".into(), ..Default::default() });
    let mut strat = ManualStrategy::new(s);
    strat.path_var = String::new();
    let p = strat.plan(&game(Store::Manual, "w")).unwrap();
    assert_eq!((p.env["PROTON_DISABLE_HIDRAW"].as_str(), p.env["PROTON_PREFER_SDL"].as_str()), ("1", "0"));
    let p = strat.plan(&game(Store::Manual, "n")).unwrap();
    assert!(!p.env.contains_key("PROTON_DISABLE_HIDRAW"));
}
