//! Measurement hooks for the rendering work (off unless the variables are set; see the build report).
//!
//! * `GRAPPLE_BENCH=<file.js>`: run that script in the main window 1.5 s after the first paint (it reports
//!   through the `ui_log` command, so results land in the process log).
//! * `GRAPPLE_BENCH_WINDOWS=<n>`: switch the 380x92 playing card on and off `n` times (window resizes, which
//!   must not end in "Error 71" on NVIDIA).

use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewWindow};

pub fn start(app: &AppHandle, window: &WebviewWindow) {
    if let Some(path) = std::env::var_os("GRAPPLE_BENCH") {
        let w = window.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            match tokio::fs::read_to_string(&path).await {
                Ok(js) => {
                    let _ = w.eval(&js);
                }
                Err(e) => eprintln!("[grapple bench] cannot read {}: {e}", path.to_string_lossy()),
            }
        });
    }
    if let Some(n) = std::env::var("GRAPPLE_BENCH_WINDOWS").ok().and_then(|v| v.parse::<u32>().ok()) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(2000)).await;
            for i in 0..n {
                if let Some(state) = app.try_state::<std::sync::Arc<crate::AppState>>() {
                    crate::windows::set_compact(&app, &state, true);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    crate::windows::set_compact(&app, &state, false);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                eprintln!("[grapple bench] window cycle {} of {n} done", i + 1);
            }
        });
    }
}
