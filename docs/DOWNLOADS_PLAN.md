# grapple downloads: build plan

Approved design: `mockups/07-downloads.html` (open it in a browser; four screens A–D). Build what it shows.
Where this file and the mockup disagree, this file wins (it records decisions made after the mockup).
The settings page in `mockups/08-settings.html` is **not** approved yet: build the Downloads settings as the
single page of mockup 07 screen D, reachable from the avatar menu, as its own component so it can move into the
settings page later.

Read first: `PLAN.md` (architecture, speed rules, Steam/Epic background), `docs/STORE_PLAN.md` (how a big
feature was split last time), `app/src-tauri/src/store.rs` (events + commands pattern to copy).

## What grapple downloads, and how

| | Steam games | Epic games |
| --- | --- | --- |
| Who downloads | **Steam**, running hidden (`-silent`). grapple never downloads Steam content itself. | **grapple itself** (new crate `grapple-download`). No Epic, Heroic or legendary process. |
| Control | The **Steam bridge** (below, opt-in): pause, resume, reorder, install and uninstall with no Steam window. Without the bridge: grapple only watches, and installs open Steam's own install dialog (`steam://install/<id>`, what `install_game` does today). | Full: queue, pause, resume, cancel, speed limit, resume after a crash/reboot. |
| Progress | Bridge: Steam's live numbers. No bridge: `appmanifest_<id>.acf` + `logs/content_log.txt` + the size of `steamapps/downloading/<id>`. | grapple's own counters. |
| After install | Steam writes the appmanifest; grapple-scan picks it up (rescan). | grapple writes legendary's `installed.json` entry and manifest file, so grapple-scan, grapple-launch **and Heroic** all see the game. |

## Rules (binding)

- Cross-platform: grapple targets Windows and every Linux desktop. Nothing may depend on Hyprland, NVIDIA or
  systemd. Paths come from `grapple_launch::paths` / the scanners, never hard-coded `/home/...`. Use
  `std::fs`/`tokio::fs` and `statvfs`-style free-space through a small cross-platform helper (`fs2` or
  `sysinfo` crate, or `libc::statvfs` on unix and `GetDiskFreeSpaceExW` on Windows). Mark code that is
  Linux-only and give it a fallback.
- Privacy and secrets, exactly as the rest of grapple: never log a token, exchange code, cookie or the Steam
  bridge's websocket URL. Epic tokens come only from `grapple_launch::epic_auth::EpicAuth::session()`. Tests never
  read the real `user.json`, never call real Epic, Steam or tether endpoints, never start Steam: they use temp dirs,
  generated fixtures and local fake HTTP/WebSocket servers (`axum` is already a dev-dependency elsewhere).
- Copy is terse, like the mockup: section titles alone ("Up next", "Updates", "Done"), no explanatory sentences.
  Green only for progress/price advantage, never decoration. Official art only (cover/hero/header/logo).
- Every colour/radius/shadow in CSS is a token from `app/ui/src/styles/tokens.css` (`pnpm check:tokens` must pass).
- The UI renders from state pushed by Rust; no polling loops in the UI. Rust pushes `downloads-changed` at most
  4 times a second while something moves, once on each state change otherwise.
- No blocking work on the main thread; commands that touch disk or network are `async`.
- Licensing: the workspace says MIT and the license decision is open. **Do not copy legendary's (GPL-3) code or
  comments.** Implement the Epic formats from the description in this file (it was verified against a real
  manifest and a real chunk on 2026-10-20). Existing grapple code that cites legendary stays as it is.

## Epic downloader (new crate `crates/grapple-download`)

Add it to the workspace (`[workspace.dependencies]` + members already glob `crates/*`). Depends on
`grapple-core`, `grapple-launch` (for `EpicAuth`, `USER_AGENT`, `FileLock`, `write_atomic`, `paths`), `reqwest`
(same features as grapple-launch), `tokio`, `flate2` (zlib), `sha1`, `serde`, `rusqlite` (bundled, like core).

### Where things are (legendary's files, shared with Heroic)

`legendary_dir` = `LaunchSettings::legendary_dir` (on Josh's machine `~/.config/heroic/legendaryConfig/legendary`;
plain legendary uses `~/.config/legendary`; on Windows `%USERPROFILE%\.config\legendary` or Heroic's equivalent).
- `assets.json`: `{"Windows": [{"app_name","asset_id","build_version","catalog_item_id","label_name":"Live-Windows","metadata":{},"namespace","sidecar_rev"}]}` — every game the account owns, with its current build.
- `metadata/<app_name>.json`: `{"app_name","app_title","asset_infos":{"Windows":{…same as an assets entry…}},"base_urls":[],"metadata":{"namespace","id" (catalog item id),"title","keyImages",…,"customAttributes":{"FolderName":{"value"},"CanRunOffline":{"value"},…}}}`.
- `installed.json`: `{ "<app_name>": InstalledGame }`, real example (Rocket League, written by Heroic/legendary):
  ```json
  {"Sugar": {
    "app_name": "Sugar",
    "base_urls": ["https://epicgames-download1.akamaized.net/Builds/Org/o-98larctxyhn55kqjq5xjb9wzjl9hf9/e6bcca5b37d0457ca881aec508205542/default",
                  "https://egs-cloudfront-chunks.epicgamescdn.com/Builds/Org/o-98larctxyhn55kqjq5xjb9wzjl9hf9/e6bcca5b37d0457ca881aec508205542/default",
                  "https://egdownload.fastly-edge.com/Builds/Org/o-98larctxyhn55kqjq5xjb9wzjl9hf9/e6bcca5b37d0457ca881aec508205542/default"],
    "can_run_offline": true, "egl_guid": "", "executable": "Binaries/Win64/Launcher.exe",
    "install_path": "/home/user/Games/Heroic/rocketleague", "install_size": 43679198548, "install_tags": [],
    "is_dlc": false, "is_preloaded": false, "launch_parameters": "", "manifest_path": null,
    "needs_verification": false, "platform": "Windows",
    "prereq_info": {"args": "install e6bcca5b37d0457ca881aec508205542", "ids": ["EAC_Service"], "name": "",
                    "path": "Binaries/Win64/EasyAntiCheat/EasyAntiCheat_EOS_Setup.exe"},
    "requires_ot": false, "save_path": null, "save_timestamp": null, "title": "Rocket League®",
    "uninstaller": {"args": "uninstall e6bcca5b37d0457ca881aec508205542", "path": "Binaries/Win64/EasyAntiCheat/EasyAntiCheat_EOS_Setup.exe"},
    "use_signed_url": false, "version": "++Prime+Update60-CL-529371"}}
  ```
  Write it under the lock file `installed.json.lock` (use `FileLock::acquire`, the same helper `user.json` uses)
  with `write_atomic`, read-modify-write, keeping every other game and every unknown field untouched.
- `manifests/<app_name>_<platform>_<build_version>.manifest`: the raw manifest bytes of an installed build
  (real names: `Sugar_Windows_++Prime+Update60-CL-529371.manifest`). Keep the old one after an update (Heroic does).

### Epic APIs (all with `Authorization: bearer <access token>` from `EpicAuth::session()`, `User-Agent: USER_AGENT`)

- Owned games + current builds: `GET https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/Windows?label=Live`
  → `[{"appName","labelName","buildVersion","catalogItemId","namespace","assetId"}]`. Use it for update checks
  (every 6 h and on demand); compare `buildVersion` with `installed.json` `version`. Fall back to `assets.json`
  when offline.
- Manifest location: `GET https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/v2/platform/Windows/namespace/{namespace}/catalogItem/{catalogItemId}/app/{appName}/label/Live`
  → `{"elements":[{"appName","labelName","buildVersion","hash":"<sha1 hex of the manifest file>","useSignedUrl":bool,"manifests":[{"uri":"https://…/<file>.manifest","queryParams":[{"name","value"}]}]}]}`.
  - Manifest URL = `uri` + (`?` + `name=value&…` when `queryParams` is non-empty).
  - **Base URLs** for chunks = every distinct `uri.rsplit_once('/').0`, in order.
  - Download the manifest from the first URL that works, check `sha1(bytes) == hash`.
  - `useSignedUrl == true`: not supported in v1. Fail with the error "This game needs Heroic to install for now"
    and record it in `docs/DOWNLOADS_PLAN.md` "Open".
- Chunks: `GET {base_url}/{chunk_dir}/{group:02}/{rolling_hash:016X}_{guid}.chunk`, **no auth header** (verified:
  the three CDNs above answer 200 without one). Try base URLs in order; on error or bad hash, retry the chunk up
  to 3 times with backoff (0.5 s, 2 s, 5 s) across the other base URLs, then fail the download (resumable).

### Manifest format (binary; all integers little-endian)

Verified on a real Rocket League manifest (version 21, 39 099 chunks, 20 937 files, every section ends exactly on its size).

```
Header (header_size bytes; 41 for current files)
  u32 magic = 0x44BEC00C
  u32 header_size
  u32 size_uncompressed
  u32 size_compressed
  [20] sha1 of the *uncompressed* body
  u8  stored_as        bit 0x1 = body is zlib-compressed
  u32 version          manifest feature version (21 in the real file)
Body (starts at header_size; zlib-decompress if stored_as & 1; check sha1 and size_uncompressed)
  ManifestMeta, ChunkDataList, FileManifestList, CustomFields — in this order. Each section starts with
  u32 section_size (counted from the section's first byte); after reading what you know, seek to start+size.
```

FString: `i32 n`; `n == 0` → ""; `n > 0` → n bytes UTF-8/ASCII including a trailing NUL; `n < 0` → `-n` UTF-16LE
code units including a trailing NUL.

```
ManifestMeta
  u32 size, u8 data_version, u32 feature_level, u8 is_file_data, u32 app_id,
  FString app_name, FString build_version, FString launch_exe, FString launch_command,
  u32 prereq_count, FString × prereq_count (prereq ids), FString prereq_name, FString prereq_path, FString prereq_args,
  if data_version >= 1: FString build_id
  if data_version >= 2: FString uninstall_action_path, FString uninstall_action_args
ChunkDataList (struct of arrays, count = n)
  u32 size, u8 version, u32 n,
  n × guid (4 × u32), n × u64 rolling_hash, n × [20] sha1, n × u8 group_num, n × u32 window_size (uncompressed
  size, 1 048 576 in practice), n × i64 file_size (size of the .chunk file on the CDN)
FileManifestList (struct of arrays, count = m)
  u32 size, u8 version, u32 m,
  m × FString filename, m × FString symlink_target, m × [20] sha1, m × u8 flags (0x1 read-only, 0x2 compressed,
  0x4 unix-executable), m × (u32 k, FString × k install_tags),
  m × (u32 parts, parts × ChunkPart),
  if version >= 1: m × (u32 has_md5, [16] md5 if has_md5 != 0), m × FString mime_type
  if version >= 2: m × [32] sha256
  ChunkPart: u32 part_size (28), guid (4 × u32), u32 offset (into the uncompressed chunk), u32 size; seek to start+part_size
  File size = sum of its parts' sizes.
CustomFields
  u32 size, u8 version, u32 n, n × FString key, n × FString value
```

- GUID text = the four u32 as `%08X` each, concatenated (e.g. `2D02FB2D40914F7B77D1CE917FD0F547`).
- `chunk_dir` by manifest `version`: `>= 15` → `ChunksV4`; `>= 6` → `ChunksV3`; `>= 3` → `ChunksV2`; else `Chunks`.
- Example real chunk path: `ChunksV4/96/8D4989EDD550784B_2D02FB2D40914F7B77D1CE917FD0F547.chunk`.
- A JSON manifest (first byte `{`) is an older form; v1 may reject it with "Old manifest format, install with
  Heroic" (write the parser only if time allows; note it in "Open").

### Chunk file format

Verified on a real chunk (header version 3, 66-byte header, zlib, sha1 matches).

```
u32 magic = 0xB1FE3AA2, u32 header_version, u32 header_size, u32 compressed_size,
guid (4 × u32), u64 rolling_hash, u8 stored_as (bit 0x1 = zlib),
if header_version >= 2: [20] sha1 of the uncompressed data, u8 hash_type (1 rolling, 2 sha1, 3 both)
if header_version >= 3: u32 uncompressed_size
data: compressed_size bytes at offset header_size
```
Verify: guid matches the request, sha1 of the uncompressed data matches the manifest's sha1 for that guid (when
hash_type has sha1), uncompressed length == window_size. The rolling hash need not be computed.

### Install algorithm (one download at a time is enough; chunks inside it are parallel)

1. **Plan.** Parse the manifest. Skip files whose `install_tags` is non-empty unless v1 selected them (v1: install
   all files with empty tags plus all tagged files; no selective install UI). Required bytes = sum of file sizes;
   refuse with "Not enough space on <drive>" when free space < required + 1 GB.
2. **Preallocate.** Create every directory and file under `<install_path>` with its final size (`set_len`).
   Symlinks: create them (unix) or skip with a warning (Windows).
3. **Download each chunk once.** Build a map chunk guid → list of (file, file_offset, chunk_offset, size)
   targets from the parts. Workers (default 8 concurrent HTTP requests, setting "connections") fetch a chunk,
   verify, write every target with positional writes (`FileExt::write_at` / `seek_write` on Windows), then drop
   it. Memory stays at roughly workers × 1 MiB. Order chunks by the first file that needs them so files finish
   early.
4. **Journal.** Append each finished chunk guid to `<install_path>/.grapple-download/journal` (fsync every few
   seconds). Resume = read the journal, skip those chunks. Pause = stop workers after their current chunk.
   Cancel of a new install = delete `<install_path>`; cancel of an update = delete the staging folder only.
5. **Verify.** Hash every file (sha1) against the manifest. Mismatch → re-download that file's chunks (once), then
   fail with "Download damaged, try again".
6. **Finalize.** Set unix-executable bits from flags (0x4) on unix. Write the manifest file and the
   `installed.json` entry (fields as in the example: `install_size` = sum of file sizes, `executable` =
   launch_exe, `launch_parameters` = launch_command, `prereq_info` from meta (`ids`, `name`, `path`, `args`),
   `base_urls`, `version` = build_version, `platform` "Windows", `can_run_offline` from metadata
   `customAttributes.CanRunOffline.value == "true"`, `requires_ot` from `customAttributes.OwnershipToken`, the
   rest default as in the example). Remove `.grapple-download`. Emit `games-changed` (the app rescans).
7. **Updates.** Download the new manifest; compare with the installed manifest (from `manifests/`): files with
   the same name and sha1 are kept, new/changed files are built into `<install_path>/.grapple-download/staging/`
   with the same algorithm, then moved over the old files (rename), deleted files removed, then the
   `installed.json` entry is updated. A cancelled update leaves the game playable.
8. **Speed limit.** One token bucket shared by all workers (bytes/s from settings; 0 = unlimited).

Default install folder for Epic: setting "Epic games go to", default = Heroic's `config.json`
`defaultSettings.defaultInstallPath` if present, else `<home>/Games`. The game folder inside it =
metadata `customAttributes.FolderName.value`, else the title with characters invalid on Windows removed.

**Running a newly installed Epic game.** grapple-launch already merges Heroic's `config.json` `defaultSettings`
with `GamesConfig/<app>.json` (`crates/grapple-launch/src/heroic.rs` `HeroicSettings::load`), so a game Heroic has
never seen still launches with Heroic's default Proton and prefix. The install sheet's "Runs with" choice is
stored in grapple's per-game settings (add `proton: Option<String>` (a Proton name from the list in "Proton
versions" below) to `GameSettings` in `app/src-tauri/src/backend/mod.rs`) and passed into the launch through
`LaunchSettings` (add a per-game override map next to `wrappers`, used before Heroic's choice). Never write
Heroic's `GamesConfig`. The prefix stays Heroic's default rule. Linux only; the row is hidden on Windows (Epic games
run natively there; the native launch path is a separate task, do not build it).

Proton versions to offer: the ones grapple-launch can already find (Heroic's `~/.config/heroic/tools/proton/*`,
umu's GE-Proton, Steam's `steamapps/common/Proton*`). Default label "GE-Proton (latest)" when present. Read-only:
no Proton downloading in this task.

## Steam bridge (opt-in, experimental; Josh chose full control on 2026-10-20)

Steam's own UI is a Chromium (CEF) app. When the empty file `<steam_root>/.cef-enable-remote-debugging` exists
when Steam starts (Linux: `~/.steam/steam/.cef-enable-remote-debugging`; Windows: in the Steam folder next to `steam.exe`, unverified), Steam opens
the Chrome DevTools Protocol on `http://127.0.0.1:8080`. Its headless target titled **SharedJSContext** has the
global `SteamClient` object with Steam's internal functions. Decky Loader and Millennium use exactly this.

Settings: **"Steam download control"** (Downloads settings, default **off**). Turning it on:
1. shows the risk once, terse: "Lets grapple pause, reorder and install Steam games. Any program on this PC can
   control Steam while it's on." with "Turn on" / "Cancel";
2. creates the file, then restarts Steam only when no Steam game runs (`steam -shutdown`, wait for the pid to
   exit, start it again hidden the way grapple-launch does), else says "Restarts Steam after your game";
3. turning it off deletes the file and restarts Steam the same way.

Connecting (new module `crates/grapple-download/src/steam_bridge.rs`, `tokio-tungstenite` as in grapple-tether):
- `GET http://127.0.0.1:8080/json/version` and `/json`; accept only when the host is loopback, the browser string
  contains "Chrome" and a target titled `SharedJSContext` has a URL whose host is `steamloopback.host`. Otherwise
  treat the bridge as unavailable (another program may be on port 8080). Never connect to a non-loopback address.
- Open the target's `webSocketDebuggerUrl`. Never log it.
- Calls: CDP `Runtime.evaluate` with `returnByValue: true`, `awaitPromise: true`. Expressions are built **only**
  from a fixed list of templates with validated integers (app ids, indexes); never interpolate strings from
  anywhere. Wrap each in `try { … } catch (e) { ({error: String(e)}) }`.
- Live updates: `Runtime.addBinding({name: "grappleDownloads"})`, then evaluate once:
  `SteamClient.Downloads.RegisterForDownloadItems((d, items) => grappleDownloads(JSON.stringify({kind:"items", d, items})))`
  and the same for `RegisterForDownloadOverview` (`kind:"overview"`). Handle `Runtime.bindingCalled`. Re-register
  after a reconnect. Reconnect with backoff when Steam restarts.
- Feature check on connect: evaluate `Object.keys(SteamClient.Downloads)` (and `.Installs`, `.InstallFolder`) and
  only enable actions whose functions exist. Steam changes this API without notice.

API (from the Millennium/steambrew typings, `@steambrew/client` 5.5.0; verify on Josh's machine):
```ts
SteamClient.Downloads.EnableAllDownloads(enable: boolean): void      // pause all / resume all
SteamClient.Downloads.PauseAppUpdate(appId: number): void
SteamClient.Downloads.ResumeAppUpdate(appId: number): void
SteamClient.Downloads.QueueAppUpdate(appId: number): void            // queue an update
SteamClient.Downloads.SetQueueIndex(appId: number, index: number): void
SteamClient.Downloads.MoveAppUpdateUp(appId: number) / MoveAppUpdateDown(appId: number): void
SteamClient.Downloads.RemoveFromDownloadList(appId: number): void
SteamClient.Downloads.RegisterForDownloadItems(cb: (isDownloading: boolean, items: DownloadItem[]) => void)
SteamClient.Downloads.RegisterForDownloadOverview(cb: (o: DownloadOverview) => void)
//   DownloadItem: appid, active, paused, completed, completed_time, queue_index, downloaded_bytes, total_bytes,
//                 buildid, target_buildid, update_error, update_result, update_type_info[]
//   DownloadOverview: update_appid, update_state ('None'|'Starting'|'Updating'|'Stopping'), paused,
//                 update_bytes_downloaded, update_bytes_to_download, update_bytes_staged, update_bytes_to_stage,
//                 update_network_bytes_per_second, update_seconds_remaining, update_is_install, update_start_time
SteamClient.Installs.OpenInstallWizard(appIds: number[]): void
SteamClient.Installs.SetAppList(appIds: number[]): void
SteamClient.Installs.SetInstallFolder(folderIndex: number): void
SteamClient.Installs.ContinueInstall(): void / CancelInstall(): void
SteamClient.Installs.GetInstallManagerInfo(): Promise<{rgAppIDs:{nAppID,lDiskSpaceRequiredBytes}[], eInstallState,
       nDiskSpaceRequired, nDiskSpaceAvailable, iInstallFolder, bCanChangeInstallFolder, eAppError, errorDetail, …}>
SteamClient.Installs.RegisterForShowInstallWizard(cb: (info: InstallMgrInfo) => void)
SteamClient.Installs.OpenUninstallWizard(appIds: number[], dontPrompt: boolean): void
SteamClient.InstallFolder.GetInstallFolders(): Promise<{nFolderIndex, bIsDefaultFolder, bIsMounted, strUsedSize, vecApps}[]>
```
Install with no Steam window (to be confirmed on Josh's machine; implement it so the sequence is easy to change):
`SetAppList([id])` → `SetInstallFolder(index)` → `ContinueInstall()`; if that does not start a download, fall back to
`OpenInstallWizard([id])` and, inside a one-shot `RegisterForShowInstallWizard` callback, `SetInstallFolder(index)`
+ `ContinueInstall()`. The size for the install sheet comes from `GetInstallManagerInfo().rgAppIDs[].lDiskSpaceRequiredBytes`
after `SetAppList`. Map grapple's drive choice to Steam's folder index by matching paths from
`libraryfolders.vdf` (the scanner reads it) with `GetInstallFolders()` order; if unsure, use Steam's default
folder. Uninstall = `OpenUninstallWizard([id], true)`.

Without the bridge (setting off, or bridge unavailable): watch only.
- Install: `steam://install/<id>` (Steam's dialog appears; that is accepted).
- Progress: poll every 2 s only while something is active (else every 30 s):
  `appmanifest_<id>.acf` (`StateFlags`, `BytesToDownload`, `BytesDownloaded`, `BytesToStage`, `BytesStaged`,
  `SizeOnDisk`), the size of `steamapps/downloading/<id>/`, and new lines in `logs/content_log.txt`
  (`<steam_root>/logs/content_log.txt`; read from the last offset, handle rotation). Speed = delta of bytes / time.
- Pause, reorder, cancel for Steam items are hidden in the UI (no buttons, no drag handle) and the queue shows
  Steam's own order.

Real `content_log.txt` lines for a demo install (use as a test fixture):
```
[2026-10-19 14:50:09] AppID 5246870 state changed : Update Required,
[2026-10-19 14:50:09] AppID 5246870 scheduler update : Priority First, not played for 1 seconds, update disabled for 0 seconds
[2026-10-19 14:50:09] AppID 5246870 state changed : Update Required,Update Queued,
[2026-10-19 14:50:09] AppID 5246870 state changed : Update Required,Update Queued,Update Running,
[2026-10-19 14:50:09] AppID 5246870 App update changed : Running Update,
[2026-10-19 14:50:09] AppID 5246870 App update changed : Running Update,Reconfiguring,
[2026-10-19 14:50:13] AppID 5246870 App update changed : Running Update,Verifying Installed,
[2026-10-19 14:50:13] AppID 5246870 App update changed : Running Update,Preallocating,
[2026-10-19 14:50:13] AppID 5246870 preallocated 0 files (0 MB) 
[2026-10-19 14:50:13] AppID 5246870 update started : download 0/394633216, store 0/0, reuse 0/0, delta 0/0, stage 0/1104001082 
[2026-10-19 14:50:13] AppID 5246870 state changed : Update Required,Update Queued,Update Running,Update Started,
[2026-10-19 14:50:13] AppID 5246870 App update changed : Running Update,Downloading,Staging,
[2026-10-19 14:51:56] AppID 5246870 App update changed : Running Update,Committing,
[2026-10-19 14:51:56] AppID 5246870 starting commit from "/data/SteamLibrary/steamapps/downloading/5246870" to "/data/SteamLibrary/steamapps/common/We Are So Cooked Demo" : 347 updated, 0 moved, 0 deleted files
[2026-10-19 14:51:56] AppID 5246870 state changed : Fully Installed,Update Queued,Update Running,
[2026-10-19 14:51:56] AppID 5246870 finished update, 1 mounted depots (BuildID 26041765) : 5246871 (6031331137422613911),
[2026-10-19 14:51:56] AppID 5246870 App update changed : None
```
The finished appmanifest (StateFlags 4 = fully installed; 6 = installed + update required; 1026 / 1030 =
updating; the scanner's `flags` module already decodes these):
```
"AppState"
{
	"appid"		"5246870"
	"name"		"We Are So Cooked Demo"
	"StateFlags"		"4"
	"installdir"		"We Are So Cooked Demo"
	"SizeOnDisk"		"1104001082"
	"buildid"		"26041765"
	"BytesToDownload"		"394704576"
	"BytesDownloaded"		"394704576"
	"BytesToStage"		"1884395143"
	"BytesStaged"		"1884395143"
	"TargetBuildID"		"26041765"
	"AutoUpdateBehavior"		"0"
}
```
Steam updates: an installed game with StateFlags containing UpdateRequired (2) is listed under "Updates". With the
bridge, "Update now" = `QueueAppUpdate` + `SetQueueIndex(id, 0)`; without, the button is hidden (Steam updates on
its own schedule). Size: acf `BytesToDownload` when non-zero, else "Checking size".

## The queue (one list for both stores)

State lives in Rust (`grapple-download` `Queue`), persisted in `$XDG_DATA_HOME/grapple/downloads.db` (own SQLite
file, WAL; do not change grapple-core's schema): table `downloads(id TEXT PRIMARY KEY /* GameId */, store, kind
TEXT /* install|update */, target_dir, position INTEGER, state TEXT, bytes_done, bytes_total, added_at,
finished_at, error)`. Epic items are driven by the downloader; Steam items mirror Steam (bridge or watcher) and
their position is Steam's.

- One Epic download runs at a time; Steam runs its own queue in parallel (it's a different process and disk
  pattern; that is fine).
- "Start now" moves an item to the top (Epic: pauses the running Epic item and starts this one; Steam with bridge:
  `SetQueueIndex(id, 0)`).
- Drag to reorder: Epic items freely among Epic items; Steam items among Steam items only with the bridge. The
  combined list shows the active item first, then queued items by position, Epic and Steam interleaved by
  `added_at`.
- "Pause all" pauses the Epic downloader and, with the bridge, `EnableAllDownloads(false)`.
- **Pause while playing** (setting, default on): when `Launcher::running()` becomes non-empty, pause the Epic
  downloader (and Steam with the bridge, `EnableAllDownloads(false)`); resume when the last game exits, only what
  grapple paused.
- **Only update while I'm not playing** (setting, default on): queued *updates* don't start while a game runs.
- **Update games automatically** (setting, default on): Epic update checks queue updates by themselves; Steam
  keeps its own behaviour.
- "Done" keeps the last 10 finished items for 24 h; "Play" on them launches through the normal launch path.
- A desktop notification "<Game> is ready" when an install finishes (setting "Tell me when a game is ready",
  default on), through the existing notification helper in `app/src-tauri/src/store.rs` (`notify-send` with the
  session environment). Windows: keep the same call site behind a `cfg` and add a TODO; do not add a new plugin.
- Errors are short and shown on the item: "Not enough space", "Download damaged, try again", "Epic sign-in
  expired, sign in again in Heroic", "Steam isn't responding".

## Tauri commands, events and types (contract; TS mirror in `app/ui/src/lib/download-types.ts`)

```ts
type DlStore = 'steam' | 'epic'
type DlState = 'queued' | 'starting' | 'downloading' | 'verifying' | 'installing' | 'paused' | 'done' | 'error'
interface DlItem {
  game: string                // GameId, e.g. "steam:252490" | "epic:0a2d9f6403244d12969e11da6713137b"
  store: DlStore; kind: 'install' | 'update'; state: DlState
  name: string; art: { hero?: string; header?: string; cover?: string }   // grapple://art/... URLs like Game art
  bytesDone: number; bytesTotal: number | null; bytesPerSec: number | null; secondsLeft: number | null
  speedHistory: number[]      // last 20 samples, bytes/s, for the sparkline
  targetDir: string; position: number; canPause: boolean; canReorder: boolean; canCancel: boolean
  error?: string; finishedAt?: number
}
interface DlSnapshot { active?: DlItem; queued: DlItem[]; updates: DlItem[]; done: DlItem[]; pausedAll: boolean
                       steamBridge: 'off' | 'on' | 'unavailable' | 'restart-pending' }
interface InstallTarget { path: string; label: string /* "Home drive" | "Data drive" | mount/drive name */;
  freeBytes: number; totalBytes: number; enough: boolean; isDefault: boolean }
interface InstallPlan { game: string; store: DlStore; sizeBytes: number | null; targets: InstallTarget[];
  protons?: { name: string; isDefault: boolean }[]   // Epic on Linux only
}
interface DlSettings { speedLimitBytes: number /* 0 = none */; connections: number; autoUpdate: boolean;
  updateOnlyWhenIdle: boolean; pauseWhilePlaying: boolean; notifyReady: boolean;
  steamDefaultDir?: string; epicDefaultDir: string; steamBridge: boolean }
```
Commands (all async, in `commands.rs`, registered in `main.rs`, real + mock backends):
`downloads_snapshot()`, `install_plan(game)`, `install_start(game, targetDir, proton?)` (replaces today's
`install_game` path for Steam and Epic; keep `install_game` as a thin alias), `download_pause(game)`,
`download_resume(game)`, `download_cancel(game)`, `download_start_now(game)`, `download_reorder(games: string[])`,
`downloads_pause_all(paused: boolean)`, `update_now(game)`, `uninstall_game(game)` (Epic: delete folder + remove
the installed.json entry, after a confirm in the UI; Steam: bridge or `steam://uninstall/<id>`),
`download_settings_get()`, `download_settings_set(settings)`, `steam_bridge_set(on)`.
Event: `downloads-changed` (payload `DlSnapshot`). After an install or uninstall finishes also emit `games-changed`.

Drives for `InstallTarget`: Steam = every library folder from `libraryfolders.vdf`; Epic = the Epic default dir
plus the parent of every existing Epic install plus the Steam library roots' mount points. Label: "Home drive"
when on the same filesystem as the home directory, "Data drive" style names from the mount point's last path
segment otherwise (Windows: the drive letter and volume label). `enough = free >= size + 1 GB`.

## UI (React, `app/ui/src/components/downloads/*`, `app/ui/src/lib/downloads*.ts`, `app/ui/src/styles/downloads.css`)

Exactly mockup 07:
- **A · Downloads page** (route `'downloads'` added to `route` in `app/ui/src/lib/ui.ts`): header "Downloads" with
  "Pause all"; the active item as the big card (hero art, store badge, state word, title, pause/cancel icon
  buttons, stats row "<done> of <total> GB downloaded · <speed> speed · <time> left · <drive>", the speed
  sparkline from `speedHistory`, progress bar); "Up next" with "<n> games · <size>" on the right and draggable
  rows (grip only when `canReorder`), row = header art, name, store badge (+ "downloads in grapple" for Epic),
  size, drive, "Start now"; "Updates" with the "Only while I'm not playing" switch on the right, rows with
  "Update now"; "Done" with "▶ Play". Empty state: just the header and "Nothing downloading".
- **B · Top bar ring**: next to the avatar while anything is active or queued: the ring fills with overall
  progress, the count badge = active + queued; click → popover with up to 3 items and "+N more · M updates" and
  "Open downloads". Add "Downloads" to the avatar menu (`SettingsMenu.tsx`) so the page is reachable when idle.
- **C · Install sheet**: replaces the current Install behaviour everywhere `installGame` is used
  (`app/ui/src/lib/library.ts`, Collection). Art on top, "Install <name>", one drive card per `InstallTarget`
  (radio, label, path in mono, free space with a small bar; amber `--warn` and disabled when not enough),
  "Download size", "Runs with" pill for Epic on Linux, Cancel / Install.
- **D · Download settings**: the rows of screen D (Speed limit, Update games automatically, Only update while
  I'm not playing, Pause downloads while I'm playing, Steam games go to, Epic games go to, Tell me when a game is
  ready) plus "Steam download control" (the bridge switch, with the confirm above).
- Mock data for UI-only work: extend `app/ui/src/lib/devbackend.ts` (browser dev) and
  `app/src-tauri/src/backend/mock.rs` with a simulated queue that moves (the mockup's Rust/Fall Guys/Valheim/ARK/
  Big Walk/Uncrashed/Brawlhalla set; art from `mockups/assets`). MOCK data never touches a store.

## Tests (no network, no Steam, no real tokens)

- `grapple-download`: a test-only manifest **writer** that builds binary manifests and chunk files from a small
  in-memory "game" (nested folders, a UTF-16 filename, an empty file, a file spanning 3 chunks, one chunk shared by
  2 files, a compressed and an uncompressed chunk, an executable flag). Round-trip parse tests; header and section
  sha/size checks; FString edge cases.
- End-to-end against a local axum fake of Epic: assets list, manifest-location (with and without queryParams),
  manifest file, CDN (two base URLs, the first failing with 500 on some chunks; one chunk served corrupt once).
  Checks: files byte-identical, installed.json entry exactly as specified and other entries untouched, manifest
  file written, journal resume after aborting mid-way, cancel cleans up, update v1→v2 (changed, added, removed
  files; unchanged files never rewritten — check mtimes), not-enough-space refusal (inject a fake free-space
  function), speed limit within ±20 %.
- Steam watcher: parse the content_log fixture and acf fixtures above into states/progress.
- Steam bridge: a fake CDP server (axum WebSocket) that serves `/json/version` and `/json`, answers
  `Runtime.evaluate`, sends `Runtime.bindingCalled`; check the target filtering (wrong title, non-loopback URL,
  wrong host → unavailable), that every expression sent is one of the templates with integer arguments, and the
  item/overview mapping.
- App: commands in mock mode; `pnpm typecheck`, `pnpm check:tokens`, `pnpm build`.

Run `nice -n 19 cargo test -p grapple-download`, `cargo test -p grapple-scan -p grapple-launch`, and
`cargo build -p grapple` (needs the Tauri Linux libraries). Don't run the full workspace test suite more than
once at the end.

## For Josh to test on his machine (write this list into the PR description, with exact commands)

1. Epic: install Fall Guys (owned, not installed) to `/data/Games`, pause, quit grapple, start it again, resume;
   launch it; check Heroic still lists it as installed.
2. Epic update: when Rocket League has an update, "Update now".
3. Steam bridge: turn on "Steam download control"; check Steam restarts hidden; `cargo run -p grapple-download --example steam_probe`
   (read-only: prints which `SteamClient.Downloads/Installs/InstallFolder` functions exist and the current
   download items; never calls an action); install a small free game with no Steam window; pause, resume, reorder
   two Steam downloads; uninstall it; turn the setting off and check the file is gone.
4. Steam without the bridge: install shows Steam's dialog; progress appears in grapple.
5. Pause while playing: start a game during a download.

## Open (fill in as you go)

- Selective install (install tags / SDL) for games like Fortnite.
- `useSignedUrl` games and JSON manifests.
- Native Windows launch of Epic games and Windows notifications (the "is ready" call site has a `cfg` + TODO).
- The no-window Steam install sequence is unverified: `INSTALL_STEPS` / `INSTALL_FALLBACK` in
  `crates/grapple-download/src/steam_bridge.rs` are the two lists to change after trying it on Josh's machine.
- `InstallFolder.GetInstallFolders()`: the path field is assumed to be `strFolderPath` (not in the typings). Without it
  grapple maps drives by the keys of `libraryfolders.vdf`, then falls back to Steam's default folder.
- The bridge sends `Runtime.enable` so `Runtime.bindingCalled` arrives for sure; if Steam's console output makes that
  costly, try without it.
- Windows: `grapple-launch` (`FileLock`, `write_atomic`, `paths`) is unix-only today, so `grapple-download`, which uses
  them, builds on Linux only until those get Windows code. grapple-download's own Windows branches (free space, drive
  labels, positional writes, no symlinks) were written but not compiled here.
- Steam's queue order without the bridge is not in its files: grapple lists Steam downloads in the order it first
  saw them.
- Epic updates rebuild whole changed files in `.grapple-download/staging` (they need that much free space); see
  "Reusing old file data" above.
