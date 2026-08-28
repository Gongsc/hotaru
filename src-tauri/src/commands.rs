use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as _;

use crate::models::{normalize_base, ClientInfo, Envelope, MonitorSnapshot, Settings};
use crate::settings;
use crate::state::AppState;
use crate::windows;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().clone()
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    let s = settings.sanitized();
    settings::save(&app, &s)?;
    *state.settings.write() = s.clone();
    state.bump_config_epoch();
    windows::sync_panel_url(&app, &s.backend_url);
    Ok(s)
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> MonitorSnapshot {
    state.snapshot.read().clone()
}

#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub node_count: usize,
    pub version: String,
    pub message: String,
}

#[tauri::command]
pub async fn test_connection(settings: Settings) -> Result<TestResult, String> {
    let s = settings.sanitized();
    let base = normalize_base(&s.backend_url)?;
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(s.accept_invalid_certs)
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(format!("{base}/api/nodes"));
    if !s.api_key.is_empty() {
        req = req.bearer_auth(&s.api_key);
    }
    let resp = req.send().await.map_err(|e| format!("无法连接后端: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(
            "HTTP 401：站点为私有模式，需要 API Key（管理后台 → 设置 → API Key）".into(),
        );
    }
    if !status.is_success() {
        return Err(format!("后端返回 HTTP {status}"));
    }
    let env: Envelope<Vec<ClientInfo>> = resp
        .json()
        .await
        .map_err(|e| format!("响应不是有效的 Komari API: {e}"))?;
    if env.status != "success" {
        return Err(format!(
            "后端返回错误: {}{}",
            env.status,
            env.message.map(|m| format!("（{m}）")).unwrap_or_default()
        ));
    }
    let count = env.data.as_ref().map(|v| v.len()).unwrap_or(0);
    let version = match client.get(format!("{base}/api/version")).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_default(),
        _ => String::new(),
    };
    Ok(TestResult {
        ok: true,
        node_count: count,
        version,
        message: format!("连接成功，发现 {count} 个节点"),
    })
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enable: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    let result = if enable {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_panel_cmd(app: AppHandle) {
    windows::open_panel(&app);
}

#[tauri::command]
pub fn open_settings_cmd(app: AppHandle) {
    windows::open_settings(&app);
}
