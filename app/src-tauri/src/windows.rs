//! The main window: show/hide, and the 380x92 "playing" card it turns into while a game runs.

use crate::AppState;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

pub const MAIN: &str = "main";
const COMPACT: (f64, f64) = (380.0, 92.0);
const MAIN_MIN: (f64, f64) = (960.0, 600.0);

pub fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(MAIN)
}

/// Show (and focus) the main window. Records the time for the show-from-hidden measurement.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = main_window(app) {
        if let Some(state) = app.try_state::<Arc<AppState>>() {
            *state.shown_at.lock().unwrap() = Some(Instant::now());
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn toggle_main(app: &AppHandle) {
    match main_window(app) {
        Some(w) if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) => {
            let _ = w.hide();
        }
        _ => show_main(app),
    }
}

/// The saved window state while the main window is the 380x92 playing card.
pub struct Saved {
    size: LogicalSize<f64>,
}

pub fn set_compact(app: &AppHandle, state: &AppState, on: bool) {
    let Some(w) = main_window(app) else { return };
    let mut saved = state.compact.lock().unwrap();
    if on {
        if saved.is_some() {
            return;
        }
        let scale = w.scale_factor().unwrap_or(1.0);
        let size = w.inner_size().map(|s| s.to_logical::<f64>(scale)).unwrap_or(LogicalSize::new(1280.0, 800.0));
        *saved = Some(Saved { size });
        let _ = w.set_min_size(None::<LogicalSize<f64>>);
        let _ = w.set_size(LogicalSize::new(COMPACT.0, COMPACT.1));
    } else if let Some(s) = saved.take() {
        let _ = w.set_min_size(Some(LogicalSize::new(MAIN_MIN.0, MAIN_MIN.1)));
        let _ = w.set_size(s.size);
    }
}

/// Buy: `invoke('store_checkout', { key, store })` opens the store window (src/store_window.rs). A listing's own
/// key (`steam:<appid>` on Steam, `epic:<ns>:<offer>` on Epic) goes straight to its checkout; for a game listed
/// under the other store's key, the store service says where Buy goes (`buy_target`).
#[tauri::command]
pub async fn store_checkout(app: AppHandle, key: String, store: String) -> Result<(), String> {
    use crate::store_window;
    if store_window::key_is_for(&key, &store) {
        return store_window::open_checkout(&app, &key, &store).await;
    }
    let t = crate::store::call(&app, "buy_target", serde_json::json!({ "key": key, "store": store })).await?;
    let field = |k: &str| t.get(k).and_then(serde_json::Value::as_str).filter(|v| !v.is_empty());
    if store == "epic"
        && let (Some(ns), Some(offer)) = (field("namespace"), field("offerId"))
    {
        return store_window::open_checkout(&app, &format!("epic:{ns}:{offer}"), "epic").await;
    }
    let url = field("url").ok_or_else(|| "no store page for this game".to_string())?;
    store_window::open_target(&app, &store, url).await
}
