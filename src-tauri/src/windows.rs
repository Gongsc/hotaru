use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::models::normalize_base;
use crate::state::AppState;

/// Open (or focus) the main window showing the Komari dashboard at the
/// configured backend URL. Falls back to settings when unconfigured.
pub fn open_panel(app: &AppHandle) {
    let raw = app.state::<AppState>().settings.read().backend_url.clone();
    let Ok(base) = normalize_base(&raw) else {
        open_settings(app);
        return;
    };
    match app.get_webview_window("main") {
        Some(window) => {
            {
                let st = app.state::<AppState>();
                let loaded = st.loaded_panel_url.lock().clone();
                if loaded.as_deref() != Some(base.as_str()) {
                    *st.loaded_panel_url.lock() = Some(base.clone());
                    // Navigate the webview without tearing down the window.
                    let js = serde_json::to_string(&base)
                        .unwrap_or_else(|_| "\"about:blank\"".into());
                    let _ = window.eval(&format!("window.location.replace({js})"));
                }
            }
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        None => {
            let _ = create_panel(app, &base);
        }
    }
}

/// If the main window is open and the backend URL changed, point it at the
/// new URL in the background (no focus steal).
pub fn sync_panel_url(app: &AppHandle, backend_url: &str) {
    let Ok(base) = normalize_base(backend_url) else { return };
    let Some(window) = app.get_webview_window("main") else { return };
    let st = app.state::<AppState>();
    let loaded = st.loaded_panel_url.lock().clone();
    if loaded.as_deref() != Some(base.as_str()) {
        *st.loaded_panel_url.lock() = Some(base.clone());
        let js =
            serde_json::to_string(&base).unwrap_or_else(|_| "\"about:blank\"".into());
        let _ = window.eval(&format!("window.location.replace({js})"));
    }
}

fn create_panel(app: &AppHandle, base: &str) -> tauri::Result<()> {
    let url: url::Url = base
        .parse()
        .map_err(|_| tauri::Error::WindowNotFound)?;
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Komari Panel")
        .inner_size(1200.0, 800.0)
        .min_inner_size(780.0, 560.0)
        .build()?;
    *app.state::<AppState>().loaded_panel_url.lock() = Some(base.to_string());
    let _ = window.set_focus();
    Ok(())
}

pub fn open_settings(app: &AppHandle) {
    match app.get_webview_window("settings") {
        Some(window) => {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        None => {
            let _ = WebviewWindowBuilder::new(
                app,
                "settings",
                WebviewUrl::App("index.html".into()),
            )
            .title("Komari Tray 设置")
            .inner_size(540.0, 760.0)
            .min_inner_size(480.0, 600.0)
            .build();
        }
    }
}
