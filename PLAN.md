# Grapple: build plan

Grapple is a universal game launcher for Windows and every Linux desktop. Development and testing happen on
Josh's machine (Omarchy / Hyprland, NVIDIA GTX 1660), but no feature may depend on Hyprland, NVIDIA or
systemd: those are optimisations with a working fallback elsewhere.
It starts Steam, Epic and any other games **without showing their launchers**, tracks playtime, and is
tied to Josh's own chat app **tether** for sign-in and "now playing".

The look is fixed by the approved mockups: `mockups/03-combined.html` (Home + Library) and
`mockups/04-collection.html` (adds Collection). Build what they show. Where code and mockup disagree, the
mockup wins unless this file says otherwise.

## Hard requirements

1. **Fast.** The window must appear instantly, and a game must start as fast as its store allows.
   All real work (scanning, launching, tracking, network) is Rust. The UI only renders.
2. **No store UI.** Steam runs hidden (`-silent`); Epic games run with no Epic/Heroic process at all.
3. **tether from day one**: sign in with tether, publish "now playing", show friends' games.
4. **Every colour and theme value lives in one token file** (see Theming), like tether.
5. Privacy like tether: never log tokens, no telemetry.

## Layout

```
grapple/
  Cargo.toml                 workspace
  crates/
    grapple-core/            shared types, SQLite store, paths, config  (contracts: do not change shapes casually)
    grapple-scan/            find games: Steam, Epic (legendary/Heroic), manual entries; art paths; imported playtime
    grapple-launch/          start games, find their processes, record sessions
    grapple-tether/          tether client: sign-in, gateway, presence, friends
  app/
    src-tauri/               Tauri 2 shell: commands, tray, single instance, art protocol
    ui/                      React 19 + Vite + TypeScript, plain CSS with tokens (like tether's apps/web)
  brand/                     logo
  mockups/                   approved designs (read-only reference)
```

## Speed rules (everyone)

- Grapple stays resident: closing the window hides it; the process keeps tracking playtime and presence.
  It autostarts at login (an XDG autostart entry, opt-in setting). Showing the window = instant.
- A second `grapple` launch reaches the running instance through the single-instance plugin and shows the
  main window. Never start a second process. (The Super+G desktop overlay was dropped on 2026-10-18; the
  in-window type-to-find palette stays.)
- The window renders on the GPU: Grapple sets `__NV_DISABLE_EXPLICIT_SYNC=1` for itself (NVIDIA + Wayland
  "Error 71" fix) and never `WEBKIT_DISABLE_DMABUF_RENDERER` (that forces CPU painting: 33 fps scrolling).
  The main window is opaque, so unpainted areas never show the desktop.
- Everything Grapple starts (Steam, Proton, games, the sign-in browser) gets Josh's session environment
  captured at startup (`grapple_launch::env::for_child()`), never Grapple's own variables, and runs outside
  Grapple's process tree (Steam: setsid + double fork; games: their own systemd user scope).
- The UI renders from the SQLite cache first, then a background rescan pushes changes (Tauri events).
- Art is served straight from the stores' own caches through a custom protocol (`grapple://art/...`),
  never copied or base64'd.
- Release profile: `lto = "thin"`, `codegen-units = 1`, `opt-level = 3`, `strip = true`.
- No blocking work on the main thread; Tauri commands that touch disk or network are async.

## Steam

Steam's DRM and online features live in the Steam client process (`steamclient.so`, reached by games through
`steam_api`). We do **not** bypass, emulate or patch it. Default strategy:

- If Steam is not running: start `steam -silent -nofriendsui` first, wait until it is ready (the pid file names a
  live `steam` that holds `~/.steam/steam.pipe`, as `steam.sh` checks; up to 90 s), then `steam -applaunch <appid>`.
  If it is running: `steam -applaunch <appid>` hands the request over in milliseconds. The launch card shows
  "Fetching game from Steam" meanwhile.
- Optional setting "Keep Steam ready": start `steam -silent` when Grapple starts, so launches skip Steam's cold start.
- Optional per-game "Close Steam when I quit": after the session ends and no other Steam game runs, `steam -shutdown`.
- A separate research task (`docs/steam-research.md`) checks whether a lighter official client can serve games
  (e.g. SteamCMD). Until it reports, the launch code keeps Steam behind a `LaunchStrategy` trait so a lighter
  strategy can be added without touching the rest.

Reading Steam data (all local files, no login):
- Names, types, icons: `~/.local/share/Steam/appcache/appinfo.vdf` (binary, v29 with a string table at the offset
  in the header). A working Python reference parser is in `docs/reference/appinfo.py`.
- Owned/known games: app folders in `appcache/librarycache/` whose appinfo type is `game`.
- Installed: `appmanifest_*.acf` in every library from `steamapps/libraryfolders.vdf`
  (Josh has `~/.local/share/Steam` and `/data/SteamLibrary`). StateFlags tell "update queued".
- Playtime / last played: `userdata/<id>/config/localconfig.vdf` → `UserLocalConfigStore.Software.Valve.Steam.apps.<appid>.{Playtime,LastPlayed}`
  (minutes, unix seconds). The account id is the folder in `userdata/`.
- Art: `appcache/librarycache/<appid>/` (files may sit in hashed subfolders): `library_600x900.jpg` or
  `library_capsule.jpg` (cover), `library_hero.jpg`, `logo.png`, and a 32px icon: a 40-hex-char `.jpg` in the folder root.
- Exclude tools/runtimes (Proton, Steam Linux Runtime, Steamworks Common, Wallpaper Engine is type "application"/tool).

Playtime for Steam games: Steam's own number is the total (it keeps counting when we launch through it).
Grapple's sessions add the detail (per-day chart, session count, longest). Never add the two together.

Finding a running Steam game: processes whose environ has `SteamAppId=<appid>` (or `SteamGameId`), or a
`reaper SteamLaunch AppId=<appid>` parent. The session ends when the last such process exits.
Detect sessions Grapple did not start too (poll /proc every 5 s, cheap: only new pids).

## Epic (no Epic or Heroic process)

Josh's Epic games are installed by Heroic, which uses legendary's files:
- Installed games: `~/.config/heroic/legendaryConfig/legendary/installed.json` (app_name, title, install_path,
  executable, launch_parameters, can_run_offline). Metadata + key art URLs: `.../legendary/metadata/<app>.json`.
- Login: `.../legendary/user.json` (access_token, refresh_token, expires_at, account_id, displayName).
- Per-game runtime from Heroic: `~/.config/heroic/GamesConfig/<app>.json` (wineVersion.bin = GE-Proton path,
  winePrefix, useGameMode, enviromentOptions, launcherArgs).
- Heroic's playtime: `~/.config/heroic/store/timestamp.json` (import as the starting total).

Launch, ported from legendary (`legendary/core.py` `get_launch_parameters`, `legendary/api/egs.py`), all in Rust:
1. If the access token expires within 10 minutes, refresh it (`POST https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/token`,
   grant_type=refresh_token, basic auth with legendary's client id/secret). **Epic rotates refresh tokens:** write
   the new tokens back to legendary's `user.json` atomically (temp file + rename), keeping every other field, so
   Heroic keeps working. Never log any token.
2. Get an exchange code (`GET .../account/api/oauth/exchange`).
3. Build the command line exactly like legendary: `-AUTH_LOGIN=unused -AUTH_PASSWORD=<exchange code> -AUTH_TYPE=exchangecode
   -epicapp=<app_name> -epicenv=Prod -EpicPortal -epicusername=<displayName> -epicuserid=<account_id> -epiclocale=en`
   plus the game's own launch_parameters.
4. Run through Proton the way Heroic does (umu-run if present at `~/.local/share/umu`, else the GE-Proton script with
   `STEAM_COMPAT_DATA_PATH`=prefix, `STEAM_COMPAT_CLIENT_INSTALL_PATH`=~/.local/share/Steam, `run`), with GameMode
   when the game's setting says so: the game's own processes are registered with `gamemoded -r<pid>` once they run
   (`gamemoderun`'s LD_PRELOAD only where `gamemoded` is missing). When umu's Steam Linux Runtime is installed
   completely, the launch sets `UMU_RUNTIME_UPDATE=0` (no download before the game); Grapple updates the runtime in
   the background while nothing runs (`grapple_launch::umu`). A download umu does need is a launch step.
5. Offline fallback when `can_run_offline` and the network is down: skip 1-2, no auth args.

## Other games

Manual entries: a name, an executable (native, AppImage, .exe through Proton, or any command), optional args/env,
art picked later (SteamGridDB can fill art later; not in v1).

## Store (grapple-core)

SQLite (rusqlite, bundled) at `$XDG_DATA_HOME/grapple/grapple.db` (WAL). Tables: `games` (cache of scan results),
`sessions` (game_id, started_at, ended_at, source = grapple|detected), `favourites`, `settings`.
Config: `$XDG_CONFIG_HOME/grapple/config.toml`. Theme overrides: `$XDG_CONFIG_HOME/grapple/theme.css`.

## tether

tether (https://app.tethr.lol) is a TypeScript monorepo (Fastify server, React web, Electron desktop). Today it has:
- Game activity: gateway op 3 PRESENCE_UPDATE with a Discord-style activity `{name, type:0, timestamps:{start},
  assets:{large_image: "steam:<appid>" | "icon:<sha256>"}}` (`packages/shared/src/games.ts`, `docs/discord-compat.md`
  "Game activity"). Icons upload via `routes/game-icons.ts`. Presence lives only in Redis.
- **No sign-in for outside apps.** We add one in tether (separate task, on a branch in a worktree of projectx):
  an authorize page ("Allow Grapple to show what you play and see what your friends play?") reached from the system
  browser, PKCE + loopback redirect to `http://127.0.0.1:<port>/callback`, returning a **scoped app token**
  (scopes: identify, presence.game, friends.read). The gateway accepts it for IDENTIFY with only those powers.
  Contract: `docs/tether-contract.md` (written by that task; grapple-tether follows it exactly).
- Grapple stores the app token in the system keyring (Secret Service via the `keyring` crate), never in plain files.
- "Show in tether" is a per-game toggle (default on) plus a global switch.

## Theming

`app/ui/src/styles/tokens.css` holds **every** colour, radius, blur, shadow and font value as custom properties,
layered like tether's `apps/web/src/styles/tokens.css`:
- `:root` / `[data-theme='dark']` surfaces, lines, text, status (values from tether's dark theme);
- `[data-accent='violet'|...]` the accent set (tether's 14 swatches);
- `[data-look='...']` presets later.
Components reference tokens only; a hard-coded colour anywhere else is a bug. At startup the Rust side reads
`theme.css` from the config dir (if present) and the UI appends it after tokens.css, so Josh can override any token.

## Who builds what

| Task | Owner | Writes to |
| --- | --- | --- |
| Shared types, workspace, this plan | lead | `Cargo.toml`, `crates/grapple-core/src/model.rs` |
| Steam research | Sonnet | `docs/steam-research.md` |
| Scanning + store | Sonnet | `crates/grapple-scan`, `crates/grapple-core` (store only) |
| Launching + tracking | Sonnet | `crates/grapple-launch` |
| Tauri app + UI | Sonnet | `app/` |
| tether sign-in + presence (server and client) | Opus | projectx branch `grapple-apps`, `crates/grapple-tether`, `docs/tether-contract.md` |
| Logo | lead | `brand/` |

Rules for every task: stay inside your folders; do not reformat others' files; run `nice -n 19 cargo build` /
`cargo test` for your crate; do not launch real games, start/stop Steam, or touch tokens on disk unless your task
says so (tests use fixtures and temp dirs); no git commits (the lead decides); report what you built, how you tested
it, and what is left.

## Crate APIs (agreed up front so tasks can work in parallel)

```rust
// grapple-core::db  (the SQLite store; `Store` is already the enum of stores, so this is `Db`)
pub struct Db;                                   // Clone + Send + Sync (internally Arc<Mutex<Connection>> or a pool)
impl Db {
    pub fn open(path: &Path) -> Result<Db>;
    pub fn open_default() -> Result<Db>;         // $XDG_DATA_HOME/grapple/grapple.db
    pub fn replace_games(&self, games: &[Game]) -> Result<()>;   // scan results; keeps favourites/sessions
    pub fn games(&self) -> Result<Vec<Game>>;
    pub fn start_session(&self, game: &GameId, at: i64, source: SessionSource) -> Result<i64>;
    pub fn end_session(&self, id: i64, at: i64) -> Result<()>;
    pub fn open_sessions(&self) -> Result<Vec<Session>>;       // to close/resume after a crash
    pub fn stats(&self, game: &GameId, store_playtime_min: u64, now: i64) -> Result<PlayStats>;
    pub fn favourites(&self) -> Result<Vec<GameId>>;
    pub fn set_favourite(&self, game: &GameId, on: bool) -> Result<()>;
    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>>;
    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()>;
}

// grapple-scan
pub struct ScanConfig { pub steam_root: Option<PathBuf>, pub heroic_config: Option<PathBuf> /* … */ }
impl ScanConfig { pub fn detect() -> ScanConfig; }
pub struct ScanResult { pub games: Vec<Game>, pub warnings: Vec<String> }
pub fn scan_all(cfg: &ScanConfig) -> ScanResult;               // sync, fast; call from spawn_blocking

// grapple-launch
pub struct Launcher;                                           // Clone; holds Db + settings
impl Launcher {
    pub fn new(db: Db, settings: LaunchSettings) -> Launcher;
    pub async fn launch(&self, game: &Game, events: tokio::sync::mpsc::Sender<LaunchEvent>) -> Result<()>;
    pub async fn stop(&self, game: &GameId) -> Result<()>;     // polite (SIGTERM tree), never SIGKILL Steam itself
    pub fn running(&self) -> Vec<(GameId, i64)>;
    pub fn spawn_watcher(&self, events: tokio::sync::mpsc::Sender<LaunchEvent>);  // detects games started elsewhere
}
```

## Windows

grapple runs on Windows 10/11 as well. Linux-only parts (Proton, umu and its runtime updates, GameMode, MangoHud,
systemd scopes, `/proc`) do not run there, and the Settings page hides their rows (the `platform` command).

How each part works on Windows:
- **Folders** (`grapple_core::dirs`): config in `%APPDATA%\grapple` (theme.css), data, the database and game logs in
  `%LOCALAPPDATA%\grapple`, the cache in `%LOCALAPPDATA%\grapple\cache`. Heroic is found in `%APPDATA%\heroic`,
  standalone legendary in `%USERPROFILE%\.config\legendary`.
- **Steam**: the folder from `HKCU\Software\Valve\Steam\SteamPath` (else the installer's
  `HKLM\SOFTWARE\WOW6432Node\Valve\Steam\InstallPath`), `steam.exe` from `SteamExe`; libraries, appinfo, localconfig and
  the library cache are read exactly as on Linux. Steam is ready when `HKCU\...\Steam\ActiveProcess` names a live
  `steam.exe` (`pid`) with a signed-in user (`ActiveUser` not 0); then `steam.exe -applaunch <id>`, cold start
  `-silent -nofriendsui` first, `steam.exe -shutdown` to close it.
- **Finding games' processes** (`grapple_launch::winproc`): another process's environment cannot be read on Windows
  (and anti-cheat protected games refuse it), so there is no `SteamAppId` marker. A process belongs to a game when its
  executable is under the game's install folder (Steam: `steamapps\common\<installdir>`; Epic: legendary's
  `install_path` or the Epic Games Launcher's `InstallLocation`), or is a hand-added game's executable (or its folder
  when that is clearly the game's own: two levels deep, outside `C:\Windows`). Two Steam apps sharing a folder are
  told apart by `HKCU\Software\Valve\Steam\RunningAppID`. Games grapple starts are also followed down their process
  tree (Toolhelp32 snapshot; start times guard against reused pids). Stop: `WM_CLOSE` to the game's windows, then
  `TerminateProcess` 10 s later for what still runs; never Steam.
- **Starting things**: detached (`DETACHED_PROCESS`, own process group, out of grapple's job object where allowed),
  with the environment grapple started with.
- **Epic**: games run directly (no Proton) with legendary's sign-in arguments; the login lock is legendary's (`flock`
  on Linux, a lock on byte 0 with `LockFileEx` on Windows, like python's filelock) and the rotated tokens are written
  back atomically. Games the **Epic Games Launcher** installed come from its manifests
  (`%PROGRAMDATA%\Epic\EpicGamesLauncher\Data\Manifests\*.item`, `UnrealEngineLauncher\LauncherInstalled.dat`) and
  start the same way when there is a legendary/Heroic login; without one grapple cannot sign them in, so it asks the
  launcher (`com.epicgames.launcher://apps/<ns>%3A<item>%3A<app>?action=launch&silent=true`), which starts in its
  tray. Epic's overlay: when a game has it switched on and `HKCU\Software\Epic Games\EOS\OverlayPath` is missing or
  stale, grapple points it at its own download; a working one (the launcher's) is never replaced.
- **Hand-added games**: `.exe`/`.bat`/`.cmd` run directly; `.lnk`/`.url` shortcuts are refused with a message (their
  target would not be tracked).
- **App**: start at login is `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`; notifications are toasts (the
  quiet gate is the same); links and folders open through `ShellExecuteW`; `grapple://` is registered at start (and by
  the installer); tokens live in the Credential Manager (keyring's Windows store); controllers through gilrs.
- **Store window**: a WebView2 view made with wry directly in a plain Tauri window: no Tauri IPC, no `grapple://`
  scheme, no grapple script, web messages off, its own user data folder; same allow-list (`store_window_win.rs`).

Type-check the Windows build from Linux (no Windows machine needed):

```
nix develop .#windows -c cargo check --target x86_64-pc-windows-gnu -p grapple --tests
nix develop .#windows -c cargo check --target x86_64-pc-windows-gnu --workspace --all-targets
```

The `windows` dev shell (flake.nix) has the same Rust release as the default shell plus the Windows standard
library (rust-overlay, pinned in flake.lock) and the MinGW C compiler for the C parts (SQLite, aws-lc). It can also
link, so the test binaries can be run headless under Wine (`cargo test --target x86_64-pc-windows-gnu --no-run`, then
`wine <test>.exe` with no `DISPLAY`/`WAYLAND_DISPLAY`; the app's tests need `WebView2Loader.dll` from the
`webview2-com-sys` build folder next to them). The real build, the tests on Windows and the installer come from CI
(`.github/workflows/windows.yml`, MSVC, run by hand or on pushes to `windows`; it publishes nothing, the installer is a
workflow artifact).
