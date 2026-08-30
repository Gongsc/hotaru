mod commands;
mod models;
mod monitor;
mod settings;
mod state;
mod tray;
mod windows;

use tauri::{Manager, WindowEvent};

use state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::open_settings(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(state::init(&handle));
            tray::create(&handle)?;
            monitor::spawn(handle.clone());
            windows::spawn_panel_watchdog_loop(&handle);
            if handle
                .state::<AppState>()
                .settings
                .read()
                .backend_url
                .trim()
                .is_empty()
            {
                windows::open_settings(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    // Closing the panel hides it; the tray keeps running.
                    api.prevent_close();
                    let _ = window.hide();
                } else if window.label() == "chart" {
                    // The popover has no decorations; always destroy it so a
                    // wedged webview can never linger.
                    api.prevent_close();
                    crate::windows::close_chart(window);
                }
            }
            WindowEvent::Focused(false) => {
                // The chart popover dismisses itself on focus loss — unless
                // the user pinned it open. The hide is delayed so a brief
                // focus flicker (the shell re-asserting after a tray click)
                // does not instantly close a freshly opened popover.
                if window.label() == "chart" {
                    let handle = window.app_handle();
                    let pinned = handle
                        .state::<AppState>()
                        .chart_pinned
                        .load(std::sync::atomic::Ordering::Relaxed);
                    if pinned {
                        return;
                    }
                    let win = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        if win.is_visible().unwrap_or(false) && !win.is_focused().unwrap_or(true) {
                            *win
                                .app_handle()
                                .state::<AppState>()
                                .chart_hidden_at
                                .lock() = Some(std::time::Instant::now());
                            crate::windows::close_chart(&win);
                        }
                    });
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::get_snapshot,
            commands::get_net_history,
            commands::test_connection,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_panel_cmd,
            commands::open_settings_cmd,
            commands::get_chart_pinned,
            commands::set_chart_pinned,
            commands::get_ping_records,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hotaru");
}
