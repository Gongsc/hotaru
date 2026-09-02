use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};

use crate::models::{normalize_base, ThemeMode};
use crate::state::AppState;

/// Logical width of the chart popover, and the height range its
/// content-driven height is clamped to.
const CHART_W: f64 = 320.0;
const CHART_MIN_H: f64 = 300.0;
const CHART_MAX_H: f64 = 900.0;
/// Vertical room left free so the popover never spans the whole screen.
const CHART_SCREEN_MARGIN: f64 = 80.0;

/// Logical size of the chart popover. Height depends on the node count so
/// the builder's initial size already matches the content (the page still
/// fine-tunes it from JS).
fn chart_logical_size(node_count: usize) -> (f64, f64) {
    let base = 224.0 + node_count as f64 * 32.0 + 46.0;
    (CHART_W, base.clamp(CHART_MIN_H, CHART_MAX_H))
}
/// Ignore tray clicks that arrive right after the popover auto-hid on blur,
/// so the same click does not instantly reopen it (toggle semantics).
const CHART_REOPEN_GUARD: std::time::Duration = std::time::Duration::from_millis(700);

fn native_theme(theme: ThemeMode) -> Option<tauri::Theme> {
    match theme {
        ThemeMode::System => None,
        ThemeMode::Light => Some(tauri::Theme::Light),
        ThemeMode::Dark => Some(tauri::Theme::Dark),
    }
}

/// Keep WebView media queries, native controls and macOS vibrancy on the
/// same appearance. On macOS this is app-wide; Windows applies it per window.
pub fn sync_theme(app: &AppHandle, theme: ThemeMode) {
    for window in app.webview_windows().values() {
        let _ = window.set_theme(native_theme(theme));
    }
}

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

/// Spawn a watchdog after panel creation: if the page hasn't shown itself
/// within 6s (load-finished triggers the show), display it anyway so the
/// window can't stay invisible; if it loaded but hung, recreate at 20s.
pub fn spawn_panel_watchdog(app: &AppHandle) {
    let handle = app.clone();
    let st = handle.state::<AppState>();
    let before = st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed);
    // 面板世代计数:用户在此看门狗存活期间关闭/重新打开面板,即视为过期
    let epoch = st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(6));
        let Some(window) = handle.get_webview_window("main") else { return };
        let st = handle.state::<AppState>();
        if st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed) != epoch {
            return; // 用户已关闭/重新打开面板,该看门狗作废
        }
        if !window.is_visible().unwrap_or(true) {
            let _ = window.show();
            let _ = window.set_focus();
        }
        std::thread::sleep(PANEL_LOAD_TIMEOUT - std::time::Duration::from_secs(6));
        let Some(window) = handle.get_webview_window("main") else { return };
        let st = handle.state::<AppState>();
        if st.panel_epoch.load(std::sync::atomic::Ordering::Relaxed) != epoch {
            return;
        }
        let loaded = st.panel_load_ms.load(std::sync::atomic::Ordering::Relaxed);
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
///
/// Always dispatches to the main thread: WebView2 controllers must be created
/// on the UI thread. Note that `run_on_main_thread` only *queues* the work
/// when the caller is off the main thread — callers reached from a webview must
/// therefore be `#[tauri::command(async)]`, or the window ends up being built
/// inside a WebView2 callback and hangs the process (see `commands.rs`).
pub fn open_panel(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        // 新的打开动作 = 新的面板世代,使旧的打开看门狗全部作废
        handle
            .state::<AppState>()
            .panel_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        open_panel_on_main(&handle);
    });
}

fn open_panel_on_main(app: &AppHandle) {
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
    let theme = app.state::<AppState>().settings.read().theme;
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Hotaru Panel")
        .theme(native_theme(theme))
        .inner_size(1200.0, 800.0)
        .min_inner_size(780.0, 560.0)
        .build()?;
    *app.state::<AppState>().loaded_panel_url.lock() = Some(base.to_string());
    let _ = window.set_focus();
    spawn_panel_watchdog(app);
    Ok(())
}

/// Open (or toggle) the chart popover anchored to the tray icon. The icon
/// rect comes from the tray click event, in physical pixels. A pinned
/// popover keeps its dragged position instead of re-anchoring. The window is
/// hidden rather than reloaded so its node data and expansion state survive
/// repeated opens.
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
                // Toggle-close without destroying the webview; reopening can
                // reuse the current node data and UI state.
                let _ = window.hide();
                return;
            }
            if !pinned {
                // The retained page may be taller because a node is expanded.
                // Anchor using its real size instead of the initial estimate.
                let current_h = window
                    .inner_size()
                    .ok()
                    .and_then(|size| {
                        window
                            .scale_factor()
                            .ok()
                            .map(|scale| size.height as f64 / scale)
                    })
                    .unwrap_or(ch);
                position_chart(app, &window, center_x, iy, iy + ih, current_h);
            }
            window
        }
        None => {
            let theme = app.state::<AppState>().settings.read().theme;
            let builder = WebviewWindowBuilder::new(app, "chart", WebviewUrl::App("chart.html".into()))
                .title("Hotaru")
                .theme(native_theme(theme))
                .inner_size(cw, ch)
                .decorations(false);
            // WKWebView otherwise paints an opaque white surface behind the
            // HTML panel, which leaks through its rounded corners on macOS.
            #[cfg(target_os = "macos")]
            let builder = builder
                .transparent(true)
                .effects(
                    EffectsBuilder::new()
                        .effect(Effect::Popover)
                        // Focus briefly flickers while the tray click is
                        // handed to the webview. Keep the material active so
                        // AppKit does not drop and restore the blur.
                        .state(EffectState::Active)
                        .radius(13.0)
                        .build(),
                )
                .initialization_script(
                    "const setNativeVibrancy=()=>document.documentElement?.classList.add('native-vibrancy');if(document.documentElement){setNativeVibrancy()}else{document.addEventListener('DOMContentLoaded',setNativeVibrancy,{once:true})}",
                );
            let Ok(window) = builder
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
/// request). Hiding keeps the page and its cached node state alive.
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
    app.state::<AppState>()
        .chart_below
        .store(below, std::sync::atomic::Ordering::Relaxed);
    let y = if below { icon_bottom + gap } else { icon_top - gap - h };
    let y = y.clamp(my, (my + mh - h).max(my));

    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
}

/// Clamp a requested logical popover height to the height range and, when the
/// monitor is known, to its logical height minus [`CHART_SCREEN_MARGIN`].
fn clamp_chart_h(logical_h: f64, monitor_logical_h: Option<f64>) -> f64 {
    let mut max_h = CHART_MAX_H;
    if let Some(mh) = monitor_logical_h {
        max_h = max_h.min(mh - CHART_SCREEN_MARGIN);
    }
    logical_h.clamp(CHART_MIN_H, CHART_MIN_H.max(max_h))
}

/// New top edge (physical px) for a popover resized from `cur_h` to
/// `target_h`. A `below` popover hangs off the menu bar and keeps its top
/// edge; the others sit above the taskbar and keep their bottom edge, so they
/// grow upwards. `monitor` is the containing monitor's `(top, height)`.
fn anchored_y(
    below: bool,
    cur_top: f64,
    cur_h: f64,
    target_h: f64,
    monitor: Option<(f64, f64)>,
) -> f64 {
    let y = if below { cur_top } else { cur_top + cur_h - target_h };
    match monitor {
        Some((my, mh)) => y.clamp(my, (my + mh - target_h).max(my)),
        None => y,
    }
}

/// Resize the popover to `logical_h`, growing away from the edge anchored to
/// the tray icon: hanging under the menu bar it keeps its top edge and extends
/// downwards, sitting above the taskbar it keeps its bottom edge and extends
/// upwards. Returns the logical height actually applied after clamping to the
/// popover's monitor. A plain `set_size` always grows downwards, which pushes
/// an expanded node card off the bottom of the screen.
pub fn resize_chart(app: &AppHandle, logical_h: f64) -> f64 {
    let Some(window) = app.get_webview_window("chart") else { return logical_h };
    let Ok(pos) = window.outer_position() else { return logical_h };
    let Ok(size) = window.inner_size() else { return logical_h };
    let scale = window.scale_factor().unwrap_or(1.0);
    let cur_h = size.height as f64;
    let below = app
        .state::<AppState>()
        .chart_below
        .load(std::sync::atomic::Ordering::Relaxed);

    // Probe the monitor at the anchored edge: that edge sits next to the tray
    // icon, so it is on-screen even when the opposite edge already overflowed.
    let anchor_y = if below { pos.y as f64 } else { pos.y as f64 + cur_h - 1.0 };
    let center_x = pos.x as f64 + size.width as f64 / 2.0;
    let monitor = monitor_containing(app, center_x, anchor_y);
    let bounds = monitor
        .as_ref()
        .map(|m| (m.position().y as f64, m.size().height as f64));
    let monitor_logical_h = monitor
        .as_ref()
        .map(|m| m.size().height as f64 / m.scale_factor());

    let applied = clamp_chart_h(logical_h, monitor_logical_h);
    let target_h = (applied * scale).round().max(1.0);
    let y = anchored_y(below, pos.y as f64, cur_h, target_h, bounds);

    let new_pos = PhysicalPosition::new(pos.x, y as i32);
    let new_size = PhysicalSize::new(size.width, target_h as u32);
    // Move before growing, shrink before moving: either order leaves the
    // window briefly overhanging the anchored screen edge otherwise.
    if target_h > cur_h {
        let _ = window.set_position(new_pos);
        let _ = window.set_size(new_size);
    } else {
        let _ = window.set_size(new_size);
        let _ = window.set_position(new_pos);
    }
    applied
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

/// Open (or focus) the settings window. Dispatches to the main thread (see
/// open_panel for why).
pub fn open_settings(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || open_settings_on_main(&handle));
}

fn open_settings_on_main(app: &AppHandle) {
    match app.get_webview_window("settings") {
        Some(window) => {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        None => {
            let theme = app.state::<AppState>().settings.read().theme;
            let _ = WebviewWindowBuilder::new(
                app,
                "settings",
                WebviewUrl::App("index.html".into()),
            )
            .title("Hotaru 设置")
            .theme(native_theme(theme))
            .inner_size(540.0, 760.0)
            .min_inner_size(480.0, 600.0)
            .build();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1080p monitor at the virtual-desktop origin: `(top, height)`.
    const SCREEN: Option<(f64, f64)> = Some((0.0, 1080.0));

    #[test]
    fn grows_downwards_when_anchored_below_the_icon() {
        // Menu-bar popover: the top edge stays put, extra height goes down.
        assert_eq!(anchored_y(true, 30.0, 300.0, 600.0, SCREEN), 30.0);
        assert_eq!(anchored_y(true, 30.0, 600.0, 300.0, SCREEN), 30.0);
    }

    #[test]
    fn grows_upwards_when_anchored_above_the_icon() {
        // Taskbar popover: the bottom edge stays at 1026, so the top moves up
        // instead of the bottom sliding off the screen.
        assert_eq!(anchored_y(false, 726.0, 300.0, 600.0, SCREEN), 426.0);
        assert_eq!(anchored_y(false, 426.0, 600.0, 300.0, SCREEN), 726.0);
    }

    #[test]
    fn keeps_the_popover_inside_its_monitor() {
        // Taller than the room above the anchored bottom edge: parked at the
        // monitor's top edge instead of being pushed past it.
        assert_eq!(anchored_y(false, 1000.0, 26.0, 900.0, SCREEN), 126.0);
        assert_eq!(anchored_y(false, 300.0, 26.0, 900.0, SCREEN), 0.0);
        // Below the menu bar with more height than the screen has left.
        assert_eq!(anchored_y(true, 900.0, 100.0, 600.0, SCREEN), 480.0);
        // A monitor shorter than the popover cannot overflow upwards either.
        assert_eq!(anchored_y(false, 100.0, 100.0, 900.0, Some((0.0, 800.0))), 0.0);
    }

    #[test]
    fn secondary_monitor_bounds_use_its_own_origin() {
        let above = Some((-1200.0, 1200.0));
        assert_eq!(anchored_y(false, -300.0, 100.0, 900.0, above), -1100.0);
        assert_eq!(anchored_y(false, -1150.0, 50.0, 900.0, above), -1200.0);
    }

    #[test]
    fn height_clamp_respects_the_monitor_in_logical_pixels() {
        assert_eq!(clamp_chart_h(500.0, Some(1080.0)), 500.0);
        assert_eq!(clamp_chart_h(200.0, Some(1080.0)), CHART_MIN_H);
        assert_eq!(clamp_chart_h(2000.0, Some(1080.0)), CHART_MAX_H);
        // 1080 physical at 150% scaling is 720 logical: the cap has to follow
        // the logical height or the popover ends up taller than the screen.
        assert_eq!(clamp_chart_h(700.0, Some(720.0)), 640.0);
        // Screens too short even for the minimum height still get it.
        assert_eq!(clamp_chart_h(700.0, Some(300.0)), CHART_MIN_H);
        assert_eq!(clamp_chart_h(2000.0, None), CHART_MAX_H);
    }

    #[test]
    fn initial_size_tracks_the_node_count() {
        assert_eq!(chart_logical_size(0), (CHART_W, CHART_MIN_H));
        assert_eq!(chart_logical_size(4), (CHART_W, 398.0));
        assert_eq!(chart_logical_size(100), (CHART_W, CHART_MAX_H));
    }
}
