//! Tauri commands: thin wrappers over `Backend` plus the window and perf helpers.

use crate::backend::{
    AccountsDto, GameDto, GameFilesDto, GameSettings, LibraryInfo, LibraryStats, ManualDraft, ProtonsDto, Settings, StorageDto, config_dir,
};
use crate::invites::{InvitesSnapshot, PlayOutcome, SendResult};
use crate::{AppState, autostart, windows};
use grapple_core::PlayStats;
use std::sync::Arc;
use tauri::{AppHandle, State, WebviewWindow};

type St<'a> = State<'a, Arc<AppState>>;
type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn list_games(state: St<'_>) -> Res<Vec<GameDto>> {
    let t = std::time::Instant::now();
    let games = state.backend.list_games().await.map_err(err)?;
    state.perf(format!(
        "list_games: {} games, {} installed, {} with cover, {:.1} ms",
        games.len(),
        games.iter().filter(|g| g.installed).count(),
        games.iter().filter(|g| g.art.cover).count(),
        t.elapsed().as_secs_f64() * 1000.0
    ));
    Ok(games)
}

#[tauri::command]
pub async fn game_stats(state: St<'_>, id: String) -> Res<PlayStats> {
    state.backend.game_stats(&id).await.map_err(err)
}

#[tauri::command]
pub async fn library_stats(state: St<'_>) -> Res<LibraryStats> {
    state.backend.library_stats().await.map_err(err)
}

#[tauri::command]
pub async fn launch(state: St<'_>, id: String) -> Res<()> {
    state.backend.launch(&id).await.map_err(err)
}

#[tauri::command]
pub async fn stop(state: St<'_>, id: String) -> Res<()> {
    state.backend.stop(&id).await.map_err(err)
}

#[tauri::command]
pub fn running(state: St<'_>) -> Vec<(String, i64)> {
    state.backend.running()
}

#[tauri::command]
pub async fn set_favourite(state: St<'_>, id: String, on: bool) -> Res<()> {
    state.backend.set_favourite(&id, on).await.map_err(err)
}

#[tauri::command]
pub async fn rescan(state: St<'_>) -> Res<()> {
    state.backend.rescan().await.map_err(err)
}

#[tauri::command]
pub fn get_settings(state: St<'_>) -> Settings {
    let mut s = state.backend.get_settings();
    s.autostart = autostart::is_enabled(); // the file is the truth
    if s.autostart {
        s.start_hidden = autostart::starts_hidden();
    }
    s
}

/// Saving can rewrite the login entry and re-read every game's choices: off the main thread.
#[tauri::command]
pub async fn set_settings(app: AppHandle, state: St<'_>, settings: Settings) -> Res<Settings> {
    let backend = state.backend.clone();
    let s = tauri::async_runtime::spawn_blocking(move || {
        let was = autostart::is_enabled();
        if settings.autostart != was || (settings.autostart && settings.start_hidden != autostart::starts_hidden()) {
            autostart::set(settings.autostart, settings.start_hidden).map_err(err)?;
        }
        backend.set_settings(settings).map_err(err)
    })
    .await
    .map_err(err)??;
    crate::controllers::set_nav(&app, s.pad_nav);
    Ok(s)
}

/// "Reset settings" (Settings › Advanced): grapple's own settings back to the defaults, start at login off.
/// Playtime, favourites, per-game choices and sign-ins stay.
#[tauri::command]
pub fn settings_reset(app: AppHandle, state: St<'_>) -> Res<Settings> {
    autostart::set(false, true).map_err(err)?;
    let s = state.backend.settings_reset().map_err(err)?;
    crate::controllers::set_nav(&app, s.pad_nav);
    Ok(s)
}

#[tauri::command]
pub fn get_game_settings(state: St<'_>, id: String) -> GameSettings {
    state.backend.get_game_settings(&id)
}

#[tauri::command]
pub fn set_game_settings(state: St<'_>, id: String, settings: GameSettings) {
    state.backend.set_game_settings(&id, settings);
}

/// The old "Install": now a thin alias of `install_start` into the game's default drive.
#[tauri::command]
pub async fn install_game(state: St<'_>, id: String) -> Res<()> {
    state.backend.install(&id).await.map_err(err)
}

// ------------------------------------------------------------------------------------------------ downloads
// docs/DOWNLOADS_PLAN.md "Tauri commands"; shapes in app/ui/src/lib/download-types.ts. Event: `downloads-changed`.

#[tauri::command]
pub async fn downloads_snapshot(state: St<'_>) -> Res<serde_json::Value> {
    state.backend.downloads_snapshot().await.map_err(err)
}

#[tauri::command]
pub async fn install_plan(state: St<'_>, game: String) -> Res<serde_json::Value> {
    state.backend.install_plan(&game).await.map_err(err)
}

/// Queue an install into `target_dir` (a path from `install_plan`); `proton`: the "Runs with" choice (Epic, Linux).
#[tauri::command]
pub async fn install_start(state: St<'_>, game: String, target_dir: String, proton: Option<String>) -> Res<()> {
    state.backend.install_start(&game, &target_dir, proton).await.map_err(err)
}

#[tauri::command]
pub async fn download_pause(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("pause", &game).await.map_err(err)
}

#[tauri::command]
pub async fn download_resume(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("resume", &game).await.map_err(err)
}

#[tauri::command]
pub async fn download_cancel(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("cancel", &game).await.map_err(err)
}

#[tauri::command]
pub async fn download_start_now(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("start_now", &game).await.map_err(err)
}

#[tauri::command]
pub async fn update_now(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("update_now", &game).await.map_err(err)
}

/// "Epic overlay" on an Epic game (downloads Epic's overlay the first time it is switched on).
#[tauri::command]
pub async fn eos_overlay_set(state: St<'_>, game: String, on: bool) -> Res<()> {
    state.backend.eos_overlay_set(&game, on).await.map_err(err)
}

/// The kinds of a game's art the user picked (`"cover" | "hero" | "logo" | "icon"`).
#[tauri::command]
pub fn art_custom(state: St<'_>, game: String) -> Vec<&'static str> {
    state.backend.art_custom(&game)
}

/// Pick a picture for one kind of a game's art. `false`: the picker was cancelled.
#[tauri::command]
pub async fn art_pick(state: St<'_>, game: String, kind: String) -> Res<bool> {
    state.backend.art_pick(&game, &kind).await.map_err(err)
}

/// Back to the store's own art for one kind.
#[tauri::command]
pub async fn art_reset(state: St<'_>, game: String, kind: String) -> Res<()> {
    state.backend.art_reset(&game, &kind).await.map_err(err)
}

/// "Check for update" on a game (its right-click menu): `"upToDate" | "available" | "queued" | "downloading"`.
#[tauri::command]
pub async fn check_update(state: St<'_>, game: String) -> Res<serde_json::Value> {
    state.backend.check_update(&game).await.map_err(err)
}

/// Epic: deletes the game's folder and its legendary entry (the UI asks first). Steam: Steam's uninstall.
#[tauri::command]
pub async fn uninstall_game(state: St<'_>, game: String) -> Res<()> {
    state.backend.download_action("uninstall", &game).await.map_err(err)
}

#[tauri::command]
pub async fn download_reorder(state: St<'_>, games: Vec<String>) -> Res<()> {
    state.backend.download_reorder(games).await.map_err(err)
}

#[tauri::command]
pub async fn downloads_pause_all(state: St<'_>, paused: bool) -> Res<()> {
    state.backend.downloads_pause_all(paused).await.map_err(err)
}

#[tauri::command]
pub async fn download_settings_get(state: St<'_>) -> Res<serde_json::Value> {
    state.backend.download_settings_get().await.map_err(err)
}

#[tauri::command]
pub async fn download_settings_set(state: St<'_>, settings: serde_json::Value) -> Res<serde_json::Value> {
    state.backend.download_settings_set(settings).await.map_err(err)
}

/// "Steam download control" (the UI confirms first).
#[tauri::command]
pub async fn steam_bridge_set(state: St<'_>, on: bool) -> Res<serde_json::Value> {
    state.backend.steam_bridge_set(on).await.map_err(err)
}

/// The file picker for "Add any game": the program's name, path and detected kind, or `null` when cancelled.
#[tauri::command]
pub async fn pick_program(state: St<'_>) -> Res<Option<serde_json::Value>> {
    state.backend.pick_program().await.map_err(err)
}

/// What a typed path would be started as.
#[tauri::command]
pub fn detect_program(state: St<'_>, path: String) -> serde_json::Value {
    serde_json::to_value(state.backend.detect_program(&path)).unwrap_or_default()
}

#[tauri::command]
pub async fn add_manual_game(state: St<'_>, draft: ManualDraft) -> Res<String> {
    state.backend.add_manual_game(draft).await.map_err(err)
}

#[tauri::command]
pub async fn remove_manual_game(state: St<'_>, id: String) -> Res<()> {
    state.backend.remove_manual_game(&id).await.map_err(err)
}

/// Signed in? Who? Which instance? Friends. Never contains a token.
#[tauri::command]
pub fn tether_status(state: St<'_>) -> Res<serde_json::Value> {
    state.backend.tether_status().map_err(err)
}

/// Opens the system browser for the tether sign-in and waits for the answer. Only ever called from the
/// "Sign in with tether" button.
#[tauri::command]
pub async fn tether_sign_in(state: St<'_>) -> Res<serde_json::Value> {
    state.backend.tether_sign_in().await.map_err(err)
}

#[tauri::command]
pub fn tether_cancel_sign_in(state: St<'_>) {
    state.backend.tether_cancel_sign_in();
}

#[tauri::command]
pub async fn tether_sign_out(state: St<'_>) -> Res<serde_json::Value> {
    state.backend.tether_sign_out().await.map_err(err)
}

/// Setting `tether.instance_url`.
#[tauri::command]
pub async fn tether_set_instance_url(state: St<'_>, url: String) -> Res<serde_json::Value> {
    state.backend.tether_set_instance_url(url).await.map_err(err)
}

// ------------------------------------------------------------------------------------------------ game invites
// mockups/09-invites.html, docs/tether-contract.md section 7. Shapes in src/invites.rs (and app/ui/src/lib/types.ts).
// Events: `invites-changed` (the list), `invite-play` (Play pressed in tether: what grapple did or the UI opens).

/// The invites of the last day (sent and received), whether invites work for this sign-in, and the deep link's
/// invite. Never a `join` payload.
#[tauri::command]
pub fn invites_snapshot(state: St<'_>) -> InvitesSnapshot {
    state.backend.invites_snapshot()
}

/// Invite friends (tether user ids) to a library game: one invite per friend, open for 15 minutes.
#[tauri::command]
pub async fn invite_send(state: St<'_>, game: String, friends: Vec<String>) -> Res<Vec<SendResult>> {
    state.backend.invite_send(&game, friends).await.map_err(err)
}

#[tauri::command]
pub async fn invite_cancel(state: St<'_>, id: String) -> Res<()> {
    state.backend.invite_cancel(&id).await.map_err(err)
}

#[tauri::command]
pub async fn invite_decline(state: St<'_>, id: String) -> Res<()> {
    state.backend.invite_decline(&id).await.map_err(err)
}

/// Invites to Josh that the Invites list just showed.
#[tauri::command]
pub async fn invites_seen(state: St<'_>, ids: Vec<String>) -> Res<()> {
    state.backend.invites_seen(ids).await.map_err(err)
}

/// Play on an invite to Josh: accepted, then launched if installed here; else the answer says what the UI opens
/// (install sheet, store page). Only ever called from a Play button the person pressed.
#[tauri::command]
pub async fn invite_accept(state: St<'_>, id: String) -> Res<PlayOutcome> {
    state.backend.invite_accept(&id).await.map_err(err)
}

/// Close the deep link's invite.
#[tauri::command]
pub fn invite_close(state: St<'_>) {
    state.backend.invite_close();
}

/// "Who can invite me": `friends` | `nobody` (kept by tether).
#[tauri::command]
pub async fn invite_settings_get(state: St<'_>) -> Res<String> {
    state.backend.invite_settings_get().await.map_err(err)
}

#[tauri::command]
pub async fn invite_settings_set(state: St<'_>, from: String) -> Res<String> {
    state.backend.invite_settings_set(&from).await.map_err(err)
}

// ------------------------------------------------------------------------------------------------ settings page
// mockups/08-settings.html. Shapes in app/ui/src/lib/types.ts.

/// `{ os: "linux" | "windows" | "macos", version }`: the page hides the rows a system does not have.
#[tauri::command]
pub fn platform(app: AppHandle) -> serde_json::Value {
    serde_json::json!({ "os": crate::system::platform(), "version": app.package_info().version.to_string() })
}

/// "Hide grapple" when a game starts (Settings › General).
#[tauri::command]
pub fn hide_main(app: AppHandle) {
    if let Some(w) = windows::main_window(&app) {
        let _ = w.hide();
    }
}

#[tauri::command]
pub async fn library_info(state: St<'_>) -> Res<LibraryInfo> {
    state.backend.library_info().await.map_err(err)
}

#[tauri::command]
pub async fn protons(state: St<'_>) -> Res<ProtonsDto> {
    state.backend.protons().await.map_err(err)
}

#[tauri::command]
pub async fn accounts(state: St<'_>) -> Res<AccountsDto> {
    state.backend.accounts().await.map_err(err)
}

/// Clears the store window's cookies (the UI asks first).
#[tauri::command]
pub async fn store_sign_out(state: St<'_>) -> Res<()> {
    state.backend.store_sign_out().await.map_err(err)
}

#[tauri::command]
pub async fn game_files(state: St<'_>, id: String) -> Res<GameFilesDto> {
    state.backend.game_files(&id).await.map_err(err)
}

/// Ids of the games whose "Show it to friends" is off.
#[tauri::command]
pub async fn games_not_shared(state: St<'_>) -> Res<Vec<String>> {
    state.backend.games_not_shared().await.map_err(err)
}

#[tauri::command]
pub async fn storage_info(state: St<'_>) -> Res<StorageDto> {
    state.backend.storage().await.map_err(err)
}

/// Empties grapple's cache folder; answers the bytes freed.
#[tauri::command]
pub async fn cache_clear(state: St<'_>) -> Res<u64> {
    state.backend.cache_clear().await.map_err(err)
}

/// Opens `data`, `logs` or a game's folder (`game` = its id) with the desktop's file manager.
#[tauri::command]
pub async fn open_place(state: St<'_>, place: String, game: Option<String>) -> Res<()> {
    state.backend.open_place(&place, game.as_deref()).await.map_err(err)
}

/// The end of the newest game log, for "Copy last launch".
#[tauri::command]
pub async fn last_launch_log(state: St<'_>) -> Res<String> {
    state.backend.last_launch_log().await.map_err(err)
}

fn theme_path() -> std::path::PathBuf {
    config_dir().join("grapple").join("theme.css")
}

/// `{ path, exists }` of theme.css (Settings › Appearance › Your own theme).
#[tauri::command]
pub async fn theme_file() -> serde_json::Value {
    let p = theme_path();
    serde_json::json!({ "path": crate::system::tilde(&p), "exists": tokio::fs::try_exists(&p).await.unwrap_or(false) })
}

/// "Create and open": theme.css with a commented starter (only when it is missing), opened with the desktop's
/// handler for .css files.
#[tauri::command]
pub async fn theme_open() -> Res<()> {
    let p = theme_path();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        crate::system::ensure_theme_file(&p)?;
        crate::system::open(&p)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

/// `$XDG_CONFIG_HOME/grapple/theme.css`, or an empty string. The UI appends it after tokens.css.
#[tauri::command]
pub async fn get_theme_override() -> String {
    tokio::fs::read_to_string(config_dir().join("grapple").join("theme.css")).await.unwrap_or_default()
}

#[tauri::command]
pub fn set_compact(app: AppHandle, state: St<'_>, on: bool) {
    windows::set_compact(&app, &state, on);
}

/// The UI has painted its first frame: show the main window (unless started hidden).
#[tauri::command]
pub async fn ui_ready(app: AppHandle, window: WebviewWindow, state: St<'_>) -> Res<()> {
    let ms = state.started.elapsed().as_secs_f64() * 1000.0;
    state.perf(format!("first paint (ui_ready) {ms:.0} ms after process start"));
    if !state.start_hidden {
        windows::show_main(&app);
    }
    state.backend.start();
    crate::bench::start(&app, &window);
    // Test hook for the MOCK backend only: start a simulated launch (and stop it) without touching the UI.
    #[cfg(all(feature = "mock", not(feature = "real")))]
    if let Some(id) = std::env::var("GRAPPLE_MOCK_LAUNCH").ok() {
        let state = state.inner().clone();
        let secs: u64 = std::env::var("GRAPPLE_MOCK_STOP_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        tauri::async_runtime::spawn(async move {
            let _ = state.backend.launch(&id).await;
            if secs > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                let _ = state.backend.stop(&id).await;
            }
        });
    }
    Ok(())
}

/// The webview painted a frame after a show(): the show-from-hidden time.
#[tauri::command]
pub fn report_shown(state: St<'_>) {
    if let Some(t) = state.shown_at.lock().unwrap().take() {
        state.perf(format!("main window: show from hidden to painted {:.1} ms", t.elapsed().as_secs_f64() * 1000.0));
    }
}

/// The page's canvas colour (from tokens.css, so it follows the look and accent): the window paints it under
/// the page, so a tile WebKit has not painted yet is never see-through.
#[tauri::command]
pub fn set_window_bg(window: WebviewWindow, state: St<'_>, r: u8, g: u8, b: u8) {
    let ok = window.set_background_color(Some(tauri::window::Color(r, g, b, 255)));
    state.perf(format!("window background #{r:02x}{g:02x}{b:02x}: {ok:?}"));
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub fn show_main(app: AppHandle) {
    windows::show_main(&app);
}

/// Errors from the page (window.onerror / unhandled rejections) land in the process log.
#[tauri::command]
pub fn ui_log(window: WebviewWindow, msg: String) {
    eprintln!("[grapple ui:{}] {}", window.label(), msg.chars().take(2000).collect::<String>());
}

// ------------------------------------------------------------------------------------------------ the store
// Thin wrappers over `crate::store::call` (the data side is the grapple-store crate). Names and shapes are the
// contract in docs/STORE_PLAN.md; arguments arrive camelCase.

#[tauri::command]
pub async fn store_front(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "front", serde_json::Value::Null).await
}

#[tauri::command]
pub async fn store_game(app: AppHandle, key: String) -> Res<serde_json::Value> {
    crate::store::call(&app, "game", serde_json::json!({ "key": key })).await
}

#[tauri::command]
pub async fn store_under(app: AppHandle, tier_minor: i64, include_hidden: Option<bool>) -> Res<serde_json::Value> {
    crate::store::call(&app, "under", serde_json::json!({ "tierMinor": tier_minor, "includeHidden": include_hidden })).await
}

/// "Popular new releases": `{ games, hidden }`; `include_hidden` is the row's "Show".
#[tauri::command]
pub async fn store_new_releases(app: AppHandle, include_hidden: Option<bool>) -> Res<serde_json::Value> {
    crate::store::call(&app, "new_releases", serde_json::json!({ "includeHidden": include_hidden })).await
}

#[tauri::command]
pub async fn store_search(app: AppHandle, q: String, filters: Option<serde_json::Value>) -> Res<serde_json::Value> {
    crate::store::call(&app, "search", serde_json::json!({ "q": q, "filters": filters })).await
}

#[tauri::command]
pub async fn store_wishlist(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "wishlist", serde_json::Value::Null).await
}

#[tauri::command]
pub async fn store_wishlist_set(app: AppHandle, key: String, on: bool) -> Res<serde_json::Value> {
    crate::store::call(&app, "wishlist_set", serde_json::json!({ "key": key, "on": on })).await
}

/// Steam-wishlist games removed from grapple's list: `[{ key, name }]`.
#[tauri::command]
pub async fn store_wishlist_hidden(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "wishlist_hidden", serde_json::Value::Null).await
}

/// Show every hidden Steam-wishlist game again; answers how many came back.
#[tauri::command]
pub async fn store_wishlist_unhide(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "wishlist_unhide", serde_json::Value::Null).await
}

#[tauri::command]
pub async fn store_notify_set(app: AppHandle, key: String, on: bool) -> Res<serde_json::Value> {
    crate::store::call(&app, "notify_set", serde_json::json!({ "key": key, "on": on })).await
}

#[tauri::command]
pub async fn store_home_picks(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "home_picks", serde_json::Value::Null).await
}

#[tauri::command]
pub async fn store_settings_get(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "settings_get", serde_json::Value::Null).await
}

#[tauri::command]
pub async fn store_settings_set(app: AppHandle, settings: serde_json::Value) -> Res<serde_json::Value> {
    crate::store::call(&app, "settings_set", serde_json::json!({ "settings": settings })).await
}

/// `[{ code, name, decimals }]`
#[tauri::command]
pub async fn store_currencies(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "currencies", serde_json::Value::Null).await
}

/// Where Steam prices come from: `{ country, currency, source }`.
#[tauri::command]
pub async fn store_region_get(app: AppHandle) -> Res<serde_json::Value> {
    crate::store::call(&app, "region_get", serde_json::Value::Null).await
}

/// Choose the Steam store country by hand (`null`/empty: automatic).
#[tauri::command]
pub async fn store_region_set(app: AppHandle, country: Option<String>) -> Res<serde_json::Value> {
    crate::store::call(&app, "region_set", serde_json::json!({ "country": country })).await
}

/// Set (or, with `null`, clear) the IsThereAnyDeal API key. The key goes to the system keyring and nowhere else;
/// the answer is the settings (`itadKey: true/false`), never the key.
#[tauri::command]
pub async fn store_itad_key_set(app: AppHandle, key: Option<String>) -> Res<serde_json::Value> {
    crate::store::call(&app, "itad_key_set", serde_json::json!({ "key": key })).await
}

/// The page (and Epic ids) Buy needs for a game at one store.
#[tauri::command]
pub async fn store_buy_target(app: AppHandle, key: String, store: String) -> Res<serde_json::Value> {
    crate::store::call(&app, "buy_target", serde_json::json!({ "key": key, "store": store })).await
}
