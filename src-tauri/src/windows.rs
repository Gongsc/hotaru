use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::models::normalize_base;
use crate::state::AppState;

/// Logical size of the chart popover (node list scrolls internally).
const CHART_SIZE: (f64, f64) = (320.0, 470.0);
/// Ignore tray clicks that arrive right after the popover auto-hid on blur,
/// so the same click does not instantly reopen it (toggle semantics).
const CHART_REOPEN_GUARD: std::time::Duration = std::time::Duration::from_millis(700);

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
        .title("Hotaru Panel")
        .inner_size(1200.0, 800.0)
        .min_inner_size(780.0, 560.0)
        .build()?;
    *app.state::<AppState>().loaded_panel_url.lock() = Some(base.to_string());
    let _ = window.set_focus();
    Ok(())
}

/// Open (or toggle) the chart popover anchored to the tray icon. The icon
/// rect comes from the tray click event, in physical pixels. A pinned
/// popover keeps its dragged position instead of re-anchoring.
pub fn open_chart(app: &AppHandle, icon_rect: (f64, f64, f64, f64)) {
    let (ix, iy, iw, ih) = icon_rect;
    let center_x = ix + iw / 2.0;

    let st = app.state::<AppState>();
    let pinned = st
        .chart_pinned
        .load(std::sync::atomic::Ordering::Relaxed);

    // The popover hides itself on blur; when the tray click caused that blur
    // (last hide < CHART_REOPEN_GUARD ago) this very click is the toggle-close.
    let just_auto_hidden = st
        .chart_hidden_at
        .lock()
        .is_some_and(|t| t.elapsed() < CHART_REOPEN_GUARD);
    if just_auto_hidden {
        return;
    }

    let window = match app.get_webview_window("chart") {
        Some(window) => {
            if window.is_visible().unwrap_or(false) {
                let _ = window.hide();
                return;
            }
            if !pinned {
                position_chart(app, &window, center_x, iy, iy + ih);
            }
            window
        }
        None => {
            let Ok(window) = WebviewWindowBuilder::new(app, "chart", WebviewUrl::App("chart.html".into()))
                .title("Hotaru")
                .inner_size(CHART_SIZE.0, CHART_SIZE.1)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .shadow(false)
                .visible(false)
                .build()
            else {
                return;
            };
            position_chart(app, &window, center_x, iy, iy + ih);
            window
        }
    };

    let _ = window.show();
    let _ = window.set_focus();
}

fn position_chart(
    app: &AppHandle,
    window: &WebviewWindow,
    center_x: f64,
    icon_top: f64,
    icon_bottom: f64,
) {
    let monitor = monitor_containing(app, center_x, icon_top);
    let Some(monitor) = monitor else { return };
    let scale = monitor.scale_factor();
    let (w, h) = (CHART_SIZE.0 * scale, CHART_SIZE.1 * scale);
    let gap = 6.0 * scale;

    let mp = monitor.position();
    let ms = monitor.size();
    let (mx, my, mw, mh) = (mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64);

    let x = (center_x - w / 2.0).clamp(mx, (mx + mw - w).max(mx));
    // Icons near the top of the screen (macOS menu bar) pop below; icons near
    // the bottom (Windows taskbar) pop above.
    let below = icon_bottom < my + mh / 2.0;
    let y = if below { icon_bottom + gap } else { icon_top - gap - h };
    let y = y.clamp(my, (my + mh - h).max(my));

    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
}

fn monitor_containing(
    app: &AppHandle,
    x: f64,
    y: f64,
) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().ok()?;
    monitors
        .into_iter()
        .find(|m| {
            let p = m.position();
            let s = m.size();
            let (mx, my) = (p.x as f64, p.y as f64);
            x >= mx && x <= mx + s.width as f64 && y >= my && y <= my + s.height as f64
        })
        .or_else(|| app.primary_monitor().ok().flatten())
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
            .title("Hotaru 设置")
            .inner_size(540.0, 760.0)
            .min_inner_size(480.0, 600.0)
            .build();
        }
    }
}
