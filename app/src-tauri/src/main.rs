// Grapple: the Tauri shell. Resident process: closing the window hides it; the tray and a second launch bring
// it back. See PLAN.md "Speed rules".
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod art;
#[cfg(feature = "real")]
mod avatars;
mod autostart;
mod backend;
mod bench;
mod commands;
mod controllers;
#[cfg(feature = "real")]
mod downloads;
// Game invites: the deep link, what Play means here, the shapes the UI gets (tether's side: tether_svc.rs).
mod invites;
#[cfg(feature = "real")]
mod manual_games;
#[cfg(feature = "real")]
mod nowplaying;
// "Nothing while I'm playing": notifications held while a game runs.
#[cfg(feature = "real")]
mod quiet;
// The store's data side glue (library, events, notifications, commands' backend).
mod store;
// The store window (Steam/Epic checkout). Not a Tauri webview: it has no IPC at all (see the module docs).
mod store_window;
// The Settings page's system helpers (platform, open a folder, cache size).
mod system;
#[cfg(feature = "real")]
mod tether_svc;
mod windows;

use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WindowEvent};

static START: OnceLock<Instant> = OnceLock::new();

pub struct AppState {
    pub backend: backend::Backend,
    pub started: Instant,
    pub start_hidden: bool,
    pub compact: Mutex<Option<windows::Saved>>,
    pub shown_at: Mutex<Option<Instant>>,
    perf: bool,
}

impl AppState {
    /// Timing lines go to stderr when GRAPPLE_PERF is set (the start-up numbers in the report).
    pub fn perf(&self, line: String) {
        if self.perf {
            eprintln!("[grapple perf +{:.0}ms] {line}", self.started.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

fn has_arg(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// `--launch <game id>` (e.g. `epic:Sugar`): start that game without showing the window, for desktop entries.
fn launch_arg(args: &[String]) -> Option<String> {
    args.iter().position(|a| a == "--launch").and_then(|i| args.get(i + 1)).cloned()
}

/// Start a game asked for on the command line. If Grapple can't (unknown id, say), show the window instead.
fn launch_from_args(app: &tauri::AppHandle, id: String) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<AppState>>().inner().clone();
        if let Err(e) = state.backend.launch(&id).await {
            tracing::warn!("--launch {id}: {e}");
            windows::show_main(&app);
        }
    });
}

/// `grapple://invite/<id>` (checked by `invites::parse_invite_link`): bring grapple up on that invite, with one
/// Play button for the person to press. A link never starts a game by itself (anything on the web can open one).
fn open_invite_link(app: &tauri::AppHandle, id: String) {
    windows::show_main(app);
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.backend.open_invite(id);
    }
}

/// Variables Grapple's own window needs, set only if the session does not set them already. Only Grapple's
/// process (and WebKit's helper processes) see them: games, Steam and everything else Grapple starts get the
/// session environment Grapple started with (`grapple_launch::env`).
///
/// WebKitGTK renders on the GPU through its DMA-BUF renderer. With NVIDIA's explicit sync on Wayland, showing a
/// second surface (it was the Super+G overlay) made GTK die with "Error 71 (Protocol error)"; the old workaround,
/// `WEBKIT_DISABLE_DMABUF_RENDERER=1`, made WebKit paint everything on the CPU (choppy scrolling, unpainted
/// tiles). Turning off explicit sync for Grapple alone keeps the GPU path and the window alive.
#[cfg(target_os = "linux")]
const OWN_ENV: &[(&str, &str)] = &[("__NV_DISABLE_EXPLICIT_SYNC", "1")];

#[cfg(target_os = "linux")]
fn set_own_env() {
    #[cfg(feature = "real")]
    grapple_launch::env::capture();
    for (k, v) in OWN_ENV {
        if std::env::var_os(k).is_some() {
            continue;
        }
        // SAFETY: called as the first thing in main, before any thread exists.
        #[cfg(feature = "real")]
        unsafe {
            grapple_launch::env::set_own(k, v)
        };
        #[cfg(not(feature = "real"))]
        unsafe {
            std::env::set_var(k, v)
        };
    }
}

/// WebKitGTK 2.54 turns the desktop's font DPI (the text scale: Josh's 0.909 on NixOS) into a zoom of the whole
/// page and draws a fractionally zoomed page wrong: the layout squashed flat, no images (2.52 did not zoom).
/// grapple sizes itself, so its own GTK settings keep the plain 96 DPI, whatever the desktop says and however GTK
/// learned it (gsettings, the settings portal, Xft). A plugin, because Tauri starts plugins after GTK and before it
/// creates the window. A `GDK_DPI_SCALE` set on purpose wins. Other apps keep the desktop's scale.
#[cfg(target_os = "linux")]
fn plain_dpi<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("plain-dpi")
        .setup(|_app, _api| {
            use gtk::prelude::*;
            const DPI: i32 = 96 * 1024;
            if std::env::var_os("GDK_DPI_SCALE").is_none()
                && let Some(settings) = gtk::Settings::default()
            {
                let keep = |s: &gtk::Settings| {
                    if s.gtk_xft_dpi() != DPI {
                        s.set_gtk_xft_dpi(DPI);
                    }
                };
                keep(&settings);
                settings.connect_gtk_xft_dpi_notify(keep);
            }
            Ok(())
        })
        .build()
}

fn main() {
    START.get_or_init(Instant::now);
    // First: remember the session environment for the games and Steam, then add what the window needs.
    #[cfg(target_os = "linux")]
    set_own_env();
    // Windows sets nothing for grapple's own window; the session environment is still captured for the games.
    #[cfg(all(feature = "real", not(target_os = "linux")))]
    grapple_launch::env::capture();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = launch_arg(&args);
    // Started by the desktop's handler for a `grapple://invite/<id>` link: show the window on that invite.
    let invite = invites::invite_link_arg(&args);
    let start_hidden = (has_arg(&args, "--hidden") || launch.is_some()) && invite.is_none();

    let builder = tauri::Builder::default()
        // Must be first. A second `grapple` hands its arguments here and exits (also how an invite link reaches the
        // running grapple: the desktop starts `grapple grapple://invite/<id>`).
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(id) = invites::invite_link_arg(&argv) {
                return open_invite_link(app, id);
            }
            match launch_arg(&argv) {
                Some(id) => launch_from_args(app, id),
                None => windows::show_main(app),
            }
        }))
        // The `grapple` URL scheme (tauri.conf.json `plugins.deep-link`): installers register it, and on Windows grapple
        // registers itself at start. The links arrive as arguments (above and in `main`), never through a listener here.
        .plugin(tauri_plugin_deep_link::init())
        .register_asynchronous_uri_scheme_protocol("grapple", art::handle)
        .invoke_handler(tauri::generate_handler![
            commands::list_games,
            commands::game_stats,
            commands::library_stats,
            commands::launch,
            commands::stop,
            commands::running,
            commands::set_favourite,
            commands::rescan,
            commands::get_settings,
            commands::set_settings,
            commands::get_game_settings,
            commands::set_game_settings,
            commands::get_theme_override,
            commands::install_game,
            commands::downloads_snapshot,
            commands::install_plan,
            commands::install_start,
            commands::download_pause,
            commands::download_resume,
            commands::download_cancel,
            commands::download_start_now,
            commands::download_reorder,
            commands::downloads_pause_all,
            commands::update_now,
            commands::check_update,
            commands::eos_overlay_set,
            commands::art_custom,
            commands::art_pick,
            commands::art_reset,
            commands::uninstall_game,
            commands::download_settings_get,
            commands::download_settings_set,
            commands::steam_bridge_set,
            commands::pick_program,
            commands::detect_program,
            commands::add_manual_game,
            commands::remove_manual_game,
            commands::tether_status,
            commands::tether_sign_in,
            commands::tether_cancel_sign_in,
            commands::tether_sign_out,
            commands::tether_set_instance_url,
            commands::invites_snapshot,
            commands::invite_send,
            commands::invite_cancel,
            commands::invite_decline,
            commands::invites_seen,
            commands::invite_accept,
            commands::invite_close,
            commands::invite_settings_get,
            commands::invite_settings_set,
            commands::set_compact,
            commands::ui_ready,
            commands::report_shown,
            commands::quit_app,
            commands::show_main,
            commands::set_window_bg,
            commands::ui_log,
            windows::store_checkout,
            controllers::controllers_list,
            controllers::controller_test_start,
            controllers::controller_test_stop,
            controllers::controller_rumble,
            controllers::steam_controller_settings,
            controllers::game_uses_proton,
            commands::store_front,
            commands::store_game,
            commands::store_under,
            commands::store_new_releases,
            commands::store_search,
            commands::store_wishlist,
            commands::store_wishlist_set,
            commands::store_wishlist_hidden,
            commands::store_wishlist_unhide,
            commands::store_notify_set,
            commands::store_home_picks,
            commands::store_settings_get,
            commands::store_settings_set,
            commands::store_currencies,
            commands::store_region_get,
            commands::store_region_set,
            commands::store_itad_key_set,
            commands::store_buy_target,
            commands::platform,
            commands::hide_main,
            commands::library_info,
            commands::protons,
            commands::accounts,
            commands::store_sign_out,
            commands::game_files,
            commands::games_not_shared,
            commands::storage_info,
            commands::cache_clear,
            commands::open_place,
            commands::last_launch_log,
            commands::theme_file,
            commands::theme_open,
            commands::settings_reset,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let backend = backend::Backend::new(&handle)?;
            app.manage(Arc::new(AppState {
                backend,
                started: *START.get().unwrap(),
                start_hidden,
                compact: Mutex::new(None),
                shown_at: Mutex::new(None),
                perf: std::env::var_os("GRAPPLE_PERF").is_some(),
            }));
            // The store service (data side): reads the library, follows Steam's wishlist, keeps prices fresh.
            store::init(&handle);
            // Game controllers (Settings › Controllers, navigating with a pad).
            controllers::init(&handle, app.state::<Arc<AppState>>().backend.get_settings().pad_nav);
            if let Some(id) = launch.clone() {
                launch_from_args(&handle, id);
            }
            if let Some(id) = invite.clone() {
                app.state::<Arc<AppState>>().backend.open_invite(id);
            }
            // A Windows build without an installer: point `grapple://` at this program (the current user only). Linux
            // gets it from the desktop entry (scripts/install-linux.sh, or the package's own).
            #[cfg(windows)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = app.deep_link().register_all() {
                    tracing::warn!("could not register the grapple:// links: {e}");
                }
            }

            let show = MenuItem::with_id(app, "show", "Show grapple", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id("grapple")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .tooltip("grapple")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => windows::show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        windows::toggle_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, ev| {
            if let WindowEvent::Focused(on) = ev {
                controllers::window_focus(window, *on);
            }
            // Close = hide: the process keeps tracking playtime and presence. Unless "Closing the window" is set to
            // quit (Settings › General).
            if let WindowEvent::CloseRequested { api, .. } = ev {
                // The store window (Windows) really closes: its view and site data go with it.
                if window.label() == store_window::WINDOW_LABEL {
                    return;
                }
                let hide = window.label() != windows::MAIN
                    || window.app_handle().try_state::<Arc<AppState>>().is_none_or(|s| s.backend.get_settings().close_hides());
                api.prevent_close();
                if hide {
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        });
    #[cfg(feature = "real")]
    let builder = builder.plugin(tauri_plugin_dialog::init());
    #[cfg(target_os = "linux")]
    let builder = builder.plugin(plain_dpi());
    builder
        .build(tauri::generate_context!())
        .expect("error while building grapple")
        .run(|app, event| {
            // Close the tether connection cleanly so friends see the game stop at once.
            if let tauri::RunEvent::Exit = event
                && let Some(state) = app.try_state::<Arc<AppState>>()
            {
                state.backend.shutdown();
            }
        });
}
