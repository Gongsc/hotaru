use parking_lot::{const_mutex, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke,
    Transform,
};

use crate::models::{
    aggregate, fmt_rate, icon_state, scoped_nodes, Aggregate, IconState,
    MonitorSnapshot, Severity,
};
use crate::state::AppState;

pub const TRAY_ID: &str = "hotaru-main-tray";

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let cache = MenuCache::build(app)?;
    let menu = cache.menu.clone();
    *MENU_CACHE.lock() = Some(cache);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::new_owned(
            draw_icon(
                &IconState { severity: Severity::Down, gauge: None, badge: false },
                icon_foreground(app),
            ),
            ICON_SIZE,
            ICON_SIZE,
        ))
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Hotaru · 正在连接后端…")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| match event {
            // Left click opens the chart popover anchored to the icon; right
            // click keeps the native menu, double click opens the panel.
            // Windows delivers a Click event for press AND release — act on
            // release only, or one physical click toggles the popover twice.
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } => {
                let (px, py) = match rect.position {
                    tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
                    tauri::Position::Logical(p) => (p.x, p.y),
                };
                let (sw, sh) = match rect.size {
                    tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
                    tauri::Size::Logical(s) => (s.width, s.height),
                };
                crate::windows::open_chart(tray.app_handle(), (px, py, sw, sh));
            }
            TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => {
                crate::windows::open_panel(tray.app_handle());
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Periodic refresh (runs on the main thread)
// ---------------------------------------------------------------------------

pub fn apply(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let st = app.state::<AppState>();
    let settings = st.settings.read().clone();
    let snap = st.snapshot.read().clone();
    let scope = scoped_nodes(&settings, &snap.nodes);
    let agg = aggregate(&scope);
    let state = icon_state(&settings, &snap);

    let _ = tray.set_icon_with_as_template(Some(tauri::image::Image::new_owned(
        draw_icon(&state, icon_foreground(app)),
        ICON_SIZE,
        ICON_SIZE,
    )), cfg!(target_os = "macos"));
    let _ = tray.set_tooltip(Some(tooltip_text(&snap, &agg)));

    #[cfg(target_os = "macos")]
    // tray-icon 0.24.x ignores `None` on macOS instead of clearing the
    // existing NSStatusBarButton title. An explicit empty string both clears
    // the stale text and makes AppKit recalculate the status item width.
    let _ = tray.set_title(Some(menu_bar_title(settings.show_menu_bar_text, &agg)));

    sync_menu(app);
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

/// Cached tray menu, built once and only patched in place afterwards (the
/// autostart check state). Swapping the menu on every tick makes Windows
/// recycle the internal item ids and clicks can hit the wrong item.
struct MenuCache {
    menu: Menu<Wry>,
    autostart: CheckMenuItem<Wry>,
}

static MENU_CACHE: Mutex<Option<MenuCache>> = const_mutex(None);

impl MenuCache {
    fn build(app: &AppHandle) -> tauri::Result<Self> {
        let open_panel = MenuItem::with_id(app, "open-panel", "打开面板", true, None::<&str>)?;
        let reload_panel = MenuItem::with_id(app, "reload-panel", "刷新面板", true, None::<&str>)?;
        let open_settings = MenuItem::with_id(app, "open-settings", "设置…", true, None::<&str>)?;

        let autostart = CheckMenuItem::with_id(
            app,
            "autostart",
            "开机自启",
            true,
            app.autolaunch().is_enabled().unwrap_or(false),
            None::<&str>,
        )?;
        let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

        let sep_bottom = PredefinedMenuItem::separator(app)?;

        let menu = Menu::new(app)?;
        menu.append(&open_panel)?;
        menu.append(&reload_panel)?;
        menu.append(&open_settings)?;
        menu.append(&autostart)?;
        menu.append(&sep_bottom)?;
        menu.append(&quit)?;

        Ok(Self { menu, autostart })
    }
}

fn sync_menu(app: &AppHandle) {
    let mut cache = MENU_CACHE.lock();
    if let Some(c) = cache.as_mut() {
        let _ = c.autostart.set_checked(app.autolaunch().is_enabled().unwrap_or(false));
    }
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().0.as_str().to_string();
    match id.as_str() {
        "quit" => app.exit(0),
        "open-panel" => crate::windows::open_panel(app),
        "reload-panel" => crate::windows::recreate_panel(app),
        "open-settings" => crate::windows::open_settings(app),
        "autostart" => {
            let launcher = app.autolaunch();
            let result = if launcher.is_enabled().unwrap_or(false) {
                launcher.disable()
            } else {
                launcher.enable()
            };
            if let Err(e) = result {
                log::error!("切换开机自启失败: {e}");
            }
            refresh(app);
        }
        _ => {}
    }
}

pub fn refresh(app: &AppHandle) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || apply(&a));
}

fn tooltip_text(snap: &MonitorSnapshot, agg: &Aggregate) -> String {
    if !snap.backend_ok {
        let err = snap.error.as_deref().unwrap_or("后端不可达");
        return format!("Hotaru · 后端不可达\n{}", truncate(err, 90));
    }
    format!(
        "Hotaru · 在线 {}/{}\nCPU {:.0}% · 内存 {:.0}%\n↑{} ↓{}",
        agg.online,
        agg.total,
        agg.cpu,
        agg.mem_pct,
        fmt_rate(agg.net_up),
        fmt_rate(agg.net_down)
    )
}

#[cfg(any(target_os = "macos", test))]
fn menu_bar_text(agg: &Aggregate) -> String {
    format!("↑{} ↓{}", fmt_rate(agg.net_up), fmt_rate(agg.net_down))
}

#[cfg(any(target_os = "macos", test))]
fn menu_bar_title(show: bool, agg: &Aggregate) -> String {
    if show {
        menu_bar_text(agg)
    } else {
        String::new()
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_menu_bar_text_is_explicitly_empty() {
        let agg = Aggregate::default();
        assert_eq!(menu_bar_title(false, &agg), "");
        assert!(!menu_bar_title(true, &agg).is_empty());
    }

    #[test]
    fn tray_icon_is_monochrome_and_states_have_distinct_shapes() {
        let color = Color::from_rgba8(0, 0, 0, 255);
        let normal = draw_icon(
            &IconState { severity: Severity::Ok, gauge: Some(42.0), badge: false },
            color,
        );
        let warning = draw_icon(
            &IconState { severity: Severity::Warn, gauge: Some(88.0), badge: false },
            color,
        );
        let offline = draw_icon(
            &IconState { severity: Severity::Down, gauge: None, badge: false },
            color,
        );

        for pixel in normal.chunks_exact(4).filter(|p| p[3] > 0) {
            assert_eq!(&pixel[..3], &[0, 0, 0]);
        }
        assert_ne!(normal, warning);
        assert_ne!(normal, offline);
        assert_ne!(warning, offline);
    }
}

// ---------------------------------------------------------------------------
// Icon drawing (tiny-skia, 32x32 RGBA)
// ---------------------------------------------------------------------------

const ICON_SIZE: u32 = 32;

fn icon_foreground(app: &AppHandle) -> Color {
    #[cfg(target_os = "windows")]
    {
        let theme = app
            .webview_windows()
            .values()
            .find_map(|window| window.theme().ok())
            .unwrap_or_else(|| match app.state::<AppState>().settings.read().theme {
                crate::models::ThemeMode::Dark => tauri::Theme::Dark,
                _ => tauri::Theme::Light,
            });
        return match theme {
            tauri::Theme::Dark => Color::from_rgba8(0xF5, 0xF5, 0xF7, 0xFF),
            _ => Color::from_rgba8(0x1D, 0x1D, 0x1F, 0xFF),
        };
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Color::from_rgba8(0, 0, 0, 0xFF)
    }
}

fn draw_icon(state: &IconState, color: Color) -> Vec<u8> {
    let Some(mut pm) = Pixmap::new(ICON_SIZE, ICON_SIZE) else {
        return vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
    };
    draw_status_symbol(&mut pm, color);

    let offline = state.severity == Severity::Down
        || (state.severity == Severity::Err && state.gauge.is_none());
    if offline {
        draw_slash(&mut pm, color);
    } else if state.badge || matches!(state.severity, Severity::Warn | Severity::Err) {
        let mut pb = PathBuilder::new();
        let radius = if state.severity == Severity::Err { 3.2 } else { 2.6 };
        pb.push_circle(26.0, 6.0, radius);
        if let Some(path) = pb.finish() {
            fill(&mut pm, &path, color);
        }
    }

    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    rgba
}

fn draw_status_symbol(pm: &mut Pixmap, color: Color) {
    let mut ring = PathBuilder::new();
    ring.push_circle(16.0, 16.0, 11.1);
    if let Some(path) = ring.finish() {
        stroke(pm, &path, color, 2.9);
    }

    let mut pulse = PathBuilder::new();
    pulse.move_to(7.55, 16.0);
    pulse.line_to(11.73, 16.0);
    pulse.line_to(13.95, 11.2);
    pulse.line_to(17.78, 20.71);
    pulse.line_to(20.53, 14.22);
    pulse.line_to(24.44, 14.22);
    if let Some(path) = pulse.finish() {
        stroke(pm, &path, color, 2.9);
    }
}

fn fill(pm: &mut Pixmap, path: &tiny_skia::Path, color: Color) {
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn stroke(pm: &mut Pixmap, path: &tiny_skia::Path, color: Color, width: f32) {
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: true,
        ..Default::default()
    };
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(path, &paint, &stroke, Transform::identity(), None);
}

fn draw_slash(pm: &mut Pixmap, color: Color) {
    let mut pb = PathBuilder::new();
    pb.move_to(7.0, 7.0);
    pb.line_to(25.0, 25.0);
    if let Some(path) = pb.finish() {
        stroke(pm, &path, color, 3.5);
    }
}
