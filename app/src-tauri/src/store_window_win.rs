//! The store window on Windows: a WebView2 view made with wry directly, in a plain Tauri window (a window without a
//! Tauri webview).
//!
//! # Why nothing in it can reach grapple
//! * The view is not a Tauri webview. Tauri adds its IPC bootstrap (`__TAURI_INTERNALS__`, the invoke key), its
//!   `ipc` handler and its URI schemes (`grapple://` is served as `http://grapple.localhost` to *every* Tauri
//!   webview on Windows) only to webviews it creates. This one is built by wry with no IPC handler (so no
//!   `window.ipc` script and no `WebMessageReceived` handler), no custom protocol and no initialization script.
//! * `IsWebMessageEnabled` is off on top of that, so `window.chrome.webview.postMessage` does not exist in its pages.
//! * The window has no capability (capabilities/default.json lists `main` only), and it has no webview Tauri knows,
//!   so no Tauri event or command can target it either.
//! * Its own WebView2 user data folder (`%LOCALAPPDATA%\grapple\store-web`): its own browser process, cookies and
//!   storage, apart from grapple's main window (Tauri keeps that in `%LOCALAPPDATA%\dev.grapple.app`).
//!
//! # The same rules as on Linux (module docs of `store_window`)
//! * The main frame may only show allow-listed https hosts, checked on `NavigationStarting` (WebView2 raises it
//!   for the main frame only, redirects included), before the request is made. A link off the stores that Josh
//!   clicked (`IsUserInitiated`, or within a few seconds of one) opens in his browser, at most once a second.
//! * Downloads are cancelled, every permission request is denied, no dev tools, no saved passwords or autofill.
//! * Popups: WebView2 cannot tell a form POST into a named window (how Steam starts 3-D Secure) from a link, so every
//!   https popup a store page opens is WebView2's own popup window (it keeps the session and `window.opener`);
//!   other popups are blocked, or opened in the browser when they are plain http links.
//!
//! Differences: the Steam purchase-box script runs in the page's own world (WebView2 has no isolated worlds; the
//! script only touches the DOM, as before), keys are WebView2's own (Alt+Left/Right, F5, Ctrl+R; no Ctrl+W), and
//! WebView2 keeps its normal disk cache (the Epic exchange code in a sign-in URL is single use).
// untested on Windows: built against wry 0.57 / WebView2 from Linux (type-checked only); see PLAN.md "Windows".

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tauri::{Manager, WindowEvent};

/// A click counts for navigations that start within this time (as on Linux).
const GESTURE_WINDOW: Duration = Duration::from_secs(5);
/// At most one system-browser open per second, whatever a page does.
const EXTERNAL_GAP: Duration = Duration::from_secs(1);

struct Shared {
    cfg: Arc<Config>,
    window: tauri::Window,
    store: Cell<&'static str>,
    last_gesture: Cell<Option<Instant>>,
    last_external: Cell<Option<Instant>>,
    focus_steam_app: Cell<Option<u32>>,
}

struct Live {
    view: wry::WebView,
    shared: Rc<Shared>,
    _context: wry::WebContext,
}

thread_local! {
    // Main thread only (wry views are not Send).
    static VIEW: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Show `nav` in the store window, creating it if needed. Call from an async task (not the main thread): Tauri makes
/// the window on its event loop and waits for it.
pub fn show(app: &AppHandle, cfg: Arc<Config>, nav: Nav) -> Result<(), String> {
    let window = match app.get_window(WINDOW_LABEL) {
        Some(w) => w,
        None => tauri::window::WindowBuilder::new(app, WINDOW_LABEL)
            .title(nav.store)
            .inner_size(1100.0, 820.0)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?,
    };
    app.run_on_main_thread(move || present(window, cfg, nav)).map_err(|e| e.to_string())
}

/// Close the store window if it is open (its view and browser process go with it). Main thread.
pub fn close() {
    let live = VIEW.with(|v| v.borrow_mut().take());
    if let Some(live) = live {
        let window = live.shared.window.clone();
        drop(live);
        let _ = window.destroy();
    }
}

fn present(window: tauri::Window, cfg: Arc<Config>, nav: Nav) {
    VIEW.with(|v| {
        let mut slot = v.borrow_mut();
        if slot.is_none() {
            match create(&window, cfg) {
                Ok(live) => *slot = Some(live),
                Err(e) => {
                    tracing::warn!("store window: {e}");
                    let _ = window.destroy();
                    return;
                }
            }
        }
        let live = slot.as_ref().unwrap();
        live.shared.store.set(nav.store);
        live.shared.focus_steam_app.set(nav.focus_steam_app);
        // A load grapple asks for counts as Josh's own (he pressed Buy).
        live.shared.last_gesture.set(Some(Instant::now()));
        let _ = window.set_title(nav.store);
        if let Err(e) = live.view.load_url(nav.url.expose()) {
            tracing::warn!("store window: could not load {:?}: {e}", nav.url);
        }
        drop(nav);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    });
}

fn create(window: &tauri::Window, cfg: Arc<Config>) -> Result<Live, String> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| format!("cannot create {}: {e}", cfg.data_dir.display()))?;
    let mut context = wry::WebContext::new(Some(cfg.data_dir.clone()));
    let shared = Rc::new(Shared {
        cfg,
        window: window.clone(),
        store: Cell::new("Store"),
        last_gesture: Cell::new(None),
        last_external: Cell::new(None),
        focus_steam_app: Cell::new(None),
    });

    let title_sh = shared.clone();
    let popup_sh = shared.clone();
    let load_sh = shared.clone();
    let view = wry::WebViewBuilder::new_with_web_context(&mut context)
        .with_devtools(false)
        .with_clipboard(false)
        .with_general_autofill_enabled(false)
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_download_started_handler(|_, _| {
            tracing::info!("store window: download refused");
            false
        })
        .with_new_window_req_handler(move |url, _features| {
            // "POST": any https popup stays in the app (see the module docs); plain http links go to the browser.
            match popup_decision(&popup_sh.cfg.allow, &url, "POST", true) {
                Popup::InApp => {
                    tracing::info!("store window: popup for {}", host_of(&url));
                    wry::NewWindowResponse::Allow
                }
                Popup::Browser => {
                    external(&popup_sh, &url);
                    wry::NewWindowResponse::Deny
                }
                Popup::Block => {
                    tracing::info!("store window: popup to {} blocked", host_of(&url));
                    wry::NewWindowResponse::Deny
                }
            }
        })
        .with_document_title_changed_handler(move |page| {
            let store = title_sh.store.get();
            let title = if page.trim().is_empty() { store.to_string() } else { format!("{store} · {page}") };
            let _ = title_sh.window.set_title(&title);
        })
        .with_on_page_load_handler(move |event, url| {
            if !matches!(event, wry::PageLoadEvent::Finished) {
                return;
            }
            let Some(appid) = load_sh.focus_steam_app.get() else { return };
            if !is_steam_app_page(&load_sh.cfg.origins.steam_store, &url, appid) {
                return;
            }
            load_sh.focus_steam_app.set(None);
            VIEW.with(|v| {
                if let Ok(slot) = v.try_borrow()
                    && let Some(live) = slot.as_ref()
                    && let Err(e) = live.view.evaluate_script(STEAM_FOCUS_JS)
                {
                    tracing::debug!("store window: purchase-box script: {e}");
                }
            });
        })
        .build(window)
        .map_err(|e| format!("cannot create the store view: {e}"))?;
    lock_down(&view, &shared).map_err(|e| format!("cannot set up the store view: {e}"))?;

    window.on_window_event(|ev| {
        if let WindowEvent::Destroyed = ev {
            // Drops the view (closing its browser process); cookies are already on disk.
            VIEW.with(|v| {
                if let Ok(mut slot) = v.try_borrow_mut() {
                    slot.take();
                }
            });
        }
    });
    Ok(Live { view, shared, _context: context })
}

/// The WebView2 settings wry does not cover, and the main-frame check with WebView2's own "user initiated" flag.
fn lock_down(view: &wry::WebView, sh: &Rc<Shared>) -> windows_core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
    use webview2_com::{NavigationStartingEventHandler, take_pwstr};
    use windows_core::{BOOL, Interface, PWSTR};
    use wry::WebViewExtWindows;

    let core = view.webview();
    // SAFETY: COM calls on the view wry just created, on its (the main) thread.
    unsafe {
        let settings = core.Settings()?;
        settings.SetIsWebMessageEnabled(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
            s4.SetIsPasswordAutosaveEnabled(false)?;
            s4.SetIsGeneralAutofillEnabled(false)?;
        }
        let sh = sh.clone();
        let mut token = 0i64;
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                let mut user = BOOL::default();
                args.IsUserInitiated(&mut user)?;
                if user.as_bool() {
                    sh.last_gesture.set(Some(Instant::now()));
                }
                let recent = sh.last_gesture.get().is_some_and(|t| t.elapsed() <= GESTURE_WINDOW);
                match main_frame_decision(&sh.cfg.allow, &uri, recent, false) {
                    MainFrame::Show => {}
                    MainFrame::Browser => {
                        args.SetCancel(true)?;
                        external(&sh, &uri);
                    }
                    MainFrame::Block => {
                        tracing::info!("store window: {} blocked", host_of(&uri));
                        args.SetCancel(true)?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

fn external(sh: &Shared, url: &str) {
    let Ok(u) = Url::parse(url) else { return };
    if !matches!(u.scheme(), "http" | "https") {
        return;
    }
    if sh.last_external.get().is_some_and(|t| t.elapsed() < EXTERNAL_GAP) {
        tracing::info!("store window: {} not opened (too many browser opens)", host_of(url));
        return;
    }
    sh.last_external.set(Some(Instant::now()));
    sh.last_gesture.set(None); // one click, one browser tab
    tracing::info!("store window: opening {} in the browser", host_of(url));
    (sh.cfg.external)(&u);
}
