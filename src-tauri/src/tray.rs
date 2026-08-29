use parking_lot::{const_mutex, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

use crate::models::{
    aggregate, fmt_bytes, fmt_rate, icon_state, pct, scoped_nodes, Aggregate, IconState,
    MonitorSnapshot, NodeSnapshot, Severity, Settings, TrayMode,
};
use crate::state::AppState;

pub const TRAY_ID: &str = "hotaru-main-tray";

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let cache = {
        let st = app.state::<AppState>();
        let settings = st.settings.read().clone();
        let snap = st.snapshot.read().clone();
        MenuCache::build(app, &settings, &snap)?
    };
    let menu = cache.menu.clone();
    *MENU_CACHE.lock() = Some(cache);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::new_owned(
            draw_icon(&IconState { severity: Severity::Down, gauge: None, badge: false }),
            ICON_SIZE,
            ICON_SIZE,
        ))
        .tooltip("Hotaru · 正在连接后端…")
        .menu(&menu)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                crate::windows::open_panel(tray.app_handle());
            }
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

    let _ = tray.set_icon(Some(tauri::image::Image::new_owned(
        draw_icon(&state),
        ICON_SIZE,
        ICON_SIZE,
    )));
    let _ = tray.set_tooltip(Some(tooltip_text(&snap, &agg)));

    #[cfg(target_os = "macos")]
    if settings.show_menu_bar_text {
        let _ = tray.set_title(Some(menu_bar_text(&agg)));
    } else {
        let _ = tray.set_title(None::<String>);
    }

    if let Err(e) = sync_menu(app, &tray, &settings, &snap) {
        log::error!("更新托盘菜单失败: {e}");
    }
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

/// Cached tray menu with handles to its dynamic items.
///
/// The native menu must NOT be rebuilt on every snapshot tick: swapping the
/// menu underneath an open popup (Windows recycles the internal item ids of
/// destroyed menus) makes the next click resolve to the wrong item — in the
/// worst case "退出", so the app appeared to quit on its own. Rebuild only
/// when the menu structure changes; patch item texts / check states in place
/// otherwise.
struct MenuCache {
    structure_key: String,
    menu: Menu<Wry>,
    header: MenuItem<Wry>,
    node_lines: Vec<(String, MenuItem<Wry>)>,
    mode_aggregate: CheckMenuItem<Wry>,
    node_checks: Vec<(String, CheckMenuItem<Wry>)>,
    autostart: CheckMenuItem<Wry>,
}

static MENU_CACHE: Mutex<Option<MenuCache>> = const_mutex(None);

/// Everything that requires creating/destroying menu items — and therefore a
/// full menu rebuild — when it changes. Node data (names, load values) is not
/// part of it: those are updated in place via `set_text`.
fn structure_key(settings: &Settings, nodes: &[NodeSnapshot]) -> String {
    let mut uuids: Vec<&str> = nodes.iter().map(|n| n.uuid.as_str()).collect();
    uuids.sort_unstable();
    format!(
        "{:?}|{}|{}",
        settings.tray_mode,
        settings.pinned_uuid,
        uuids.join(","),
    )
}

impl MenuCache {
    fn build(app: &AppHandle, settings: &Settings, snap: &MonitorSnapshot) -> tauri::Result<Self> {
        let scope = scoped_nodes(settings, &snap.nodes);
        let agg = aggregate(&scope);

        let header = MenuItem::with_id(
            app,
            "header",
            header_text(snap, &agg),
            false,
            None::<&str>,
        )?;

        let mut node_lines = Vec::new();
        for n in &scope {
            node_lines.push((
                n.uuid.clone(),
                MenuItem::with_id(app, format!("info-{}", n.uuid), node_line(n), false, None::<&str>)?,
            ));
        }

        let open_panel = MenuItem::with_id(app, "open-panel", "打开面板", true, None::<&str>)?;
        let open_settings = MenuItem::with_id(app, "open-settings", "设置…", true, None::<&str>)?;

        let mode = Submenu::with_id(app, "mode-sub", "托盘数据源", true)?;
        let mode_aggregate = CheckMenuItem::with_id(
            app,
            "mode-aggregate",
            "汇总全部节点",
            true,
            settings.tray_mode == TrayMode::Aggregate,
            None::<&str>,
        )?;
        mode.append(&mode_aggregate)?;
        let mut node_checks = Vec::new();
        for n in &scope {
            let item = CheckMenuItem::with_id(
                app,
                format!("node-{}", n.uuid),
                format!("关注：{}", truncate(&n.name, 20)),
                true,
                settings.tray_mode == TrayMode::Node && settings.pinned_uuid == n.uuid,
                None::<&str>,
            )?;
            mode.append(&item)?;
            node_checks.push((n.uuid.clone(), item));
        }

        let autostart = CheckMenuItem::with_id(
            app,
            "autostart",
            "开机自启",
            true,
            app.autolaunch().is_enabled().unwrap_or(false),
            None::<&str>,
        )?;
        let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

        let sep_header = PredefinedMenuItem::separator(app)?;
        let sep_nodes = PredefinedMenuItem::separator(app)?;
        let sep_actions = PredefinedMenuItem::separator(app)?;
        let sep_bottom = PredefinedMenuItem::separator(app)?;

        let menu = Menu::new(app)?;
        menu.append(&header)?;
        menu.append(&sep_header)?;
        for (_, item) in &node_lines {
            menu.append(item)?;
        }
        menu.append(&sep_nodes)?;
        menu.append(&open_panel)?;
        menu.append(&open_settings)?;
        menu.append(&sep_actions)?;
        menu.append(&mode)?;
        menu.append(&autostart)?;
        menu.append(&sep_bottom)?;
        menu.append(&quit)?;

        Ok(Self {
            structure_key: structure_key(settings, &snap.nodes),
            menu,
            header,
            node_lines,
            mode_aggregate,
            node_checks,
            autostart,
        })
    }
}

fn sync_menu(
    app: &AppHandle,
    tray: &TrayIcon<Wry>,
    settings: &Settings,
    snap: &MonitorSnapshot,
) -> tauri::Result<()> {
    let key = structure_key(settings, &snap.nodes);
    let mut cache = MENU_CACHE.lock();

    if let Some(c) = cache.as_mut() {
        if c.structure_key == key {
            let scope = scoped_nodes(settings, &snap.nodes);
            let agg = aggregate(&scope);
            let _ = c.header.set_text(header_text(snap, &agg));
            for n in &scope {
                if let Some((_, item)) = c.node_lines.iter().find(|(u, _)| u == &n.uuid) {
                    let _ = item.set_text(node_line(n));
                }
                if let Some((_, chk)) = c.node_checks.iter().find(|(u, _)| u == &n.uuid) {
                    let _ = chk.set_text(format!("关注：{}", truncate(&n.name, 20)));
                    let _ = chk.set_checked(
                        settings.tray_mode == TrayMode::Node && settings.pinned_uuid == n.uuid,
                    );
                }
            }
            let _ = c.mode_aggregate.set_checked(settings.tray_mode == TrayMode::Aggregate);
            let _ = c.autostart.set_checked(app.autolaunch().is_enabled().unwrap_or(false));
            return Ok(());
        }
    }

    let fresh = MenuCache::build(app, settings, snap)?;
    let _ = tray.set_menu(Some(fresh.menu.clone()));
    *cache = Some(fresh);
    Ok(())
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().0.as_str().to_string();
    match id.as_str() {
        "quit" => app.exit(0),
        "open-panel" => crate::windows::open_panel(app),
        "open-settings" => crate::windows::open_settings(app),
        "mode-aggregate" => {
            app.state::<AppState>().settings.write().tray_mode = TrayMode::Aggregate;
            refresh(app);
        }
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
        other => {
            if let Some(uuid) = other.strip_prefix("node-") {
                let st = app.state::<AppState>();
                {
                    let mut s = st.settings.write();
                    s.tray_mode = TrayMode::Node;
                    s.pinned_uuid = uuid.to_string();
                }
                refresh(app);
            }
        }
    }
}

fn refresh(app: &AppHandle) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || apply(&a));
}

fn header_text(snap: &MonitorSnapshot, agg: &Aggregate) -> String {
    if !snap.backend_ok {
        return format!("⚠ {}", snap.error.as_deref().unwrap_or("后端不可达"));
    }
    format!(
        "在线 {}/{} · CPU {:.0}% · 内存 {:.0}% · ↑{} ↓{}",
        agg.online,
        agg.total,
        agg.cpu,
        agg.mem_pct,
        fmt_rate(agg.net_up),
        fmt_rate(agg.net_down)
    )
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

#[cfg(target_os = "macos")]
fn menu_bar_text(agg: &Aggregate) -> String {
    format!("↑{} ↓{}", fmt_rate(agg.net_up), fmt_rate(agg.net_down))
}

fn node_line(n: &NodeSnapshot) -> String {
    if !n.online {
        return format!("⚠ {} · 离线", truncate(&n.name, 18));
    }
    let mem = pct(n.ram_used, n.ram_total)
        .map(|p| format!("内存 {:.0}%", p))
        .unwrap_or_else(|| format!("内存 {}", fmt_bytes(n.ram_used)));
    format!(
        "{} · CPU {:.0}% · {} · ↑{} ↓{}",
        truncate(&n.name, 18),
        n.cpu_usage,
        mem,
        fmt_rate(n.net_up),
        fmt_rate(n.net_down)
    )
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

// ---------------------------------------------------------------------------
// Icon drawing (tiny-skia, 32x32 RGBA)
// ---------------------------------------------------------------------------

const ICON_SIZE: u32 = 32;

fn draw_icon(state: &IconState) -> Vec<u8> {
    let Some(mut pm) = Pixmap::new(ICON_SIZE, ICON_SIZE) else {
        return vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
    };
    let track = Color::from_rgba8(0x94, 0xA3, 0xB8, 0x59);
    draw_ring(&mut pm, 16.0, 16.0, 12.5, 3.4, -90.0, 270.0, track);

    if let Some(gauge) = state.gauge {
        let color = match state.severity {
            Severity::Ok => Color::from_rgba8(0x34, 0xD3, 0x99, 0xFF),
            Severity::Warn => Color::from_rgba8(0xFB, 0xBF, 0x24, 0xFF),
            Severity::Err => Color::from_rgba8(0xF8, 0x71, 0x71, 0xFF),
            Severity::Down => Color::from_rgba8(0x94, 0xA3, 0xB8, 0xFF),
        };
        let span = (360.0 * gauge.clamp(0.0, 100.0) / 100.0) as f32;
        if span >= 1.0 {
            draw_ring(&mut pm, 16.0, 16.0, 12.5, 3.4, -90.0, -90.0 + span, color);
        }
        // small center dot
        let mut pb = PathBuilder::new();
        pb.push_circle(16.0, 16.0, 1.8);
        if let Some(path) = pb.finish() {
            fill(&mut pm, &path, color);
        }
    } else if state.severity == Severity::Down {
        draw_slash(&mut pm, Color::from_rgba8(0x9C, 0xA3, 0xAF, 0xE6));
    }

    if state.badge {
        let mut pb = PathBuilder::new();
        pb.push_circle(25.5, 6.5, 5.2);
        if let Some(path) = pb.finish() {
            fill(&mut pm, &path, Color::from_rgba8(0x1F, 0x29, 0x37, 0xD9));
        }
        let mut pb = PathBuilder::new();
        pb.push_circle(25.5, 6.5, 3.8);
        if let Some(path) = pb.finish() {
            fill(&mut pm, &path, Color::from_rgba8(0xF8, 0x71, 0x71, 0xFF));
        }
    }

    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    rgba
}

fn fill(pm: &mut Pixmap, path: &tiny_skia::Path, color: Color) {
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// Filled donut sector (annulus arc) from start_deg to end_deg.
fn draw_ring(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, w: f32, start_deg: f32, end_deg: f32, color: Color) {
    let path = donut_path(cx, cy, r + w / 2.0, r - w / 2.0, start_deg, end_deg);
    if let Some(path) = path {
        fill(pm, &path, color);
    }
}

fn donut_path(cx: f32, cy: f32, r_out: f32, r_in: f32, start_deg: f32, end_deg: f32) -> Option<tiny_skia::Path> {
    if end_deg - start_deg < 0.5 {
        return None;
    }
    let span = end_deg - start_deg;
    let steps = ((span / 4.0).ceil() as usize).max(1);
    let mut pb = PathBuilder::new();
    for i in 0..=steps {
        let t = (start_deg + span * i as f32 / steps as f32).to_radians();
        let (x, y) = (cx + r_out * t.cos(), cy + r_out * t.sin());
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    for i in (0..=steps).rev() {
        let t = (start_deg + span * i as f32 / steps as f32).to_radians();
        pb.line_to(cx + r_in * t.cos(), cy + r_in * t.sin());
    }
    pb.close();
    pb.finish()
}

fn draw_slash(pm: &mut Pixmap, color: Color) {
    let (x0, y0, x1, y1) = (9.0f32, 9.0f32, 23.0f32, 23.0f32);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    let (nx, ny) = (-dy / len * 1.75, dx / len * 1.75);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + nx, y0 + ny);
    pb.line_to(x1 + nx, y1 + ny);
    pb.line_to(x1 - nx, y1 - ny);
    pb.line_to(x0 - nx, y0 - ny);
    pb.close();
    if let Some(path) = pb.finish() {
        fill(pm, &path, color);
    }
}
