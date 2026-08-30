use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::models::normalize_base;
use crate::state::AppState;

/// Logical size of the chart popover. Height depends on the node count so
/// the builder's initial size already matches the content (the page still
/// fine-tunes it from JS).
fn chart_logical_size(node_count: usize) -> (f64, f64) {
    let base = 224.0 + node_count as f64 * 32.0 + 46.0;
    (320.0, base.clamp(300.0, 900.0))
}
/// Ignore tray clicks that arrive right after the popover auto-hid on blur,
/// so the same click does not instantly reopen it (toggle semantics).
const CHART_REOPEN_GUARD: std::time::Duration = std::time::Duration::from_millis(700);

/// Recreate the panel webview from scratch. Recover even from a crashed
/// renderer, where an in-page `location.reload()` would never run. The new
/// window is created on the main thread after a short delay so the old
/// WebView2 instance is fully torn down first.
pub fn recreate_panel(app: &AppHandle) {
    let base = app.state::<AppState>().loaded_panel_url.lock().clone();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.destroy();
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if let Some(base) = base {
                let _ = create_panel(&inner, &base);
            } else {
                open_panel(&inner);
            }
        });
    });
}

/// If the panel's external dashboard did not finish loading within this
/// window of time, reload it once (white-screen self-healing).
const PANEL_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn now_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mark the panel webview as loaded (called from on_page_load).
pub fn mark_panel_loaded(app: &AppHandle) {
    let ms = now_ms_u64();
    let st = app.state::<AppState>();
    st.panel_load_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// Spawn a one-shot watchdog: if no successful page load was recorded after
/// `PANEL_LOAD_TIMEOUT`, reload the panel once. Started on every open/create.
pub fn spawn_panel_watchdog(app: &AppHandle) {
    let handle = app.clone();
    let before = handle
        .state::<AppState>()
        .panel_load_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(PANEL_LOAD_TIMEOUT);
        let Some(window) = handle.get_webview_window("main") else { return };
        let loaded = handle
            .state::<AppState>()
            .panel_load_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        if loaded > before || !window.is_visible().unwrap_or(false) {
            return;
        }
        let _ = window.eval("location.reload()");
    });
}

/// Periodic panel health check: while the panel is visible, a navigation
/// that started but never finished within 30s triggers one reload (with a
/// per-reload cooldown). Runs forever from a background thread.
pub fn panel_watchdog_tick(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    if !window.is_visible().unwrap_or(false) {
        return;
    }
    let st = app.state::<AppState>();
    let now = now_ms_u64();
    let started = st.panel_load_started_ms.load(std::sync::atomic::Ordering::Relaxed);
    let loaded = st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed);
    let last_reload = st.panel_reload_ms.load(std::sync::atomic::Ordering::Relaxed);
    if started > loaded
        && now.saturating_sub(started) > 30_000
        && now.saturating_sub(last_reload) > 60_000
    {
        // 熔断:连续重建 3 次仍未加载成功就停止,避免无限循环
        let streak = st
            .panel_recreate_streak
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if streak >= 3 {
            return;
        }
        st.panel_reload_ms
            .store(now, std::sync::atomic::Ordering::Relaxed);
        // 页面可能已挂死,eval 的 reload 未必能执行——直接重建面板 webview。
        recreate_panel(app);
    }
}

/// Start the periodic watchdog thread (once, from setup).
pub fn spawn_panel_watchdog_loop(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(15));
        panel_watchdog_tick(&handle);
    });
}

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
            spawn_panel_watchdog(app);
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
        .on_page_load(|webview, payload| {
            let app = webview.app_handle();
            let st = app.state::<AppState>();
            let now = now_ms_u64();
            match payload.event() {
                tauri::webview::PageLoadEvent::Started => st
                    .panel_load_started_ms
                    .store(now, std::sync::atomic::Ordering::Relaxed),
                tauri::webview::PageLoadEvent::Finished => {
                    st.panel_load_ms
                        .store(now, std::sync::atomic::Ordering::Relaxed);
                    // 加载成功,重置重建熔断计数
                    st.panel_recreate_streak
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
        .build()?;
    *app.state::<AppState>().loaded_panel_url.lock() = Some(base.to_string());
    let _ = window.set_focus();
    spawn_panel_watchdog(app);
    Ok(())
}

/// Open (or toggle) the chart popover anchored to the tray icon. The icon
/// rect comes from the tray click event, in physical pixels. A pinned
/// popover keeps its dragged position instead of re-anchoring. Closing
/// DESTROYS the window so a wedged webview can never linger: every open
/// starts with a fresh one.
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

    let node_count = st.snapshot.read().nodes.len();
    let (cw, ch) = chart_logical_size(node_count);

    let window = match app.get_webview_window("chart") {
        Some(window) => {
            if window.is_visible().unwrap_or(false) {
                // Toggle-close: hide is a native op and always works, even
                // with a wedged webview. Reopening reloads the page, which
                // self-heals it — no destroy/create race involved.
                let _ = window.hide();
                return;
            }
            if !pinned {
                position_chart(app, &window, center_x, iy, iy + ih, ch);
            }
            // Reopen after hide: reload the local page so a hung webview
            // recovers instead of showing a frozen popover.
            let _ = window.eval("location.reload()");
            let _ = window.set_size(tauri::LogicalSize::new(cw, ch));
            window
        }
        None => {
            let Ok(window) = WebviewWindowBuilder::new(app, "chart", WebviewUrl::App("chart.html".into()))
                .title("Hotaru")
                .inner_size(cw, ch)
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
            position_chart(app, &window, center_x, iy, iy + ih, ch);
            window
        }
    };

    let _ = window.show();
    let _ = window.set_focus();
}

/// Close the chart popover from the Rust side (blur timeout / close
/// request). Hide is a native operation and always works; the popover page
/// reloads itself on the next open.
pub fn close_chart(window: &tauri::Window) {
    let _ = window.hide();
}

fn position_chart(
    app: &AppHandle,
    window: &WebviewWindow,
    center_x: f64,
    icon_top: f64,
    icon_bottom: f64,
    logical_h: f64,
) {
    let monitor = monitor_containing(app, center_x, icon_top);
    let Some(monitor) = monitor else { return };
    let scale = monitor.scale_factor();
    let (w, h) = (320.0 * scale, logical_h * scale);
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
