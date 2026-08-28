use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::models::Settings;

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    let Some(path) = config_path(app) else {
        return Settings::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Settings>(&text) {
            Ok(s) => s.sanitized(),
            Err(e) => {
                log::warn!("settings.json 解析失败，使用默认配置: {e}");
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let Some(path) = config_path(app) else {
        return Err("无法定位配置目录".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text =
        serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}
