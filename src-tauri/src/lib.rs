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
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    // Closing the panel hides it; the tray keeps running.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::get_snapshot,
            commands::test_connection,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_panel_cmd,
            commands::open_settings_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("error while running komari-tray");
}
