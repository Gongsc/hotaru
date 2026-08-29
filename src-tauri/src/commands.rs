use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as _;

use crate::models::{normalize_base, now_ms, ClientInfo, Envelope, MonitorSnapshot, NetPoint, Settings};
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

/// Target point count per series returned by `get_net_history`; larger
/// histories are bucket-averaged so the JSON stays small.
const HISTORY_MAX_SERIES_POINTS: usize = 720;

#[derive(Serialize)]
pub struct NetHistoryPayload {
    pub aggregate: Vec<NetPoint>,
    pub nodes: BTreeMap<String, Vec<NetPoint>>,
}

#[tauri::command]
pub fn get_net_history(state: State<'_, AppState>, range_secs: u64) -> NetHistoryPayload {
    let range_secs = range_secs.clamp(60, crate::state::HISTORY_RETENTION_MS / 1000);
    let since = now_ms().saturating_sub(range_secs * 1000);
    let frames = state.net_history.since(since);

    let mut nodes: BTreeMap<String, Vec<NetPoint>> = BTreeMap::new();
    for f in &frames {
        for (uuid, up, down) in &f.nodes {
            nodes.entry(uuid.clone()).or_default().push(NetPoint {
                t: f.t,
                up: *up,
                down: *down,
            });
        }
    }

    NetHistoryPayload {
        aggregate: downsample(&frames.iter().map(|f| NetPoint {
            t: f.t,
            up: f.nodes.iter().map(|(_, up, _)| up).sum(),
            down: f.nodes.iter().map(|(_, _, down)| down).sum(),
        }).collect::<Vec<_>>()),
        nodes: nodes
            .into_iter()
            .map(|(uuid, pts)| (uuid, downsample(&pts)))
            .collect(),
    }
}

/// Bucket-average a series down to at most `HISTORY_MAX_SERIES_POINTS` points.
fn downsample(points: &[NetPoint]) -> Vec<NetPoint> {
    if points.len() <= HISTORY_MAX_SERIES_POINTS {
        return points.to_vec();
    }
    let step = points.len().div_ceil(HISTORY_MAX_SERIES_POINTS);
    points
        .chunks(step)
        .map(|chunk| {
            let n = chunk.len() as f64;
            NetPoint {
                t: chunk[chunk.len() / 2].t,
                up: chunk.iter().map(|p| p.up).sum::<f64>() / n,
                down: chunk.iter().map(|p| p.down).sum::<f64>() / n,
            }
        })
        .collect()
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

#[tauri::command]
pub fn set_chart_pinned(state: State<'_, AppState>, pinned: bool) {
    state
        .chart_pinned
        .store(pinned, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub fn get_chart_pinned(state: State<'_, AppState>) -> bool {
    state
        .chart_pinned
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: u64, up: f64) -> NetPoint {
        NetPoint { t, up, down: up }
    }

    #[test]
    fn downsample_short_series_unchanged() {
        let pts: Vec<NetPoint> = (0..100).map(|i| pt(i * 1000, i as f64)).collect();
        assert_eq!(downsample(&pts), pts);
    }

    #[test]
    fn downsample_buckets_average() {
        let pts: Vec<NetPoint> = (0..1000).map(|i| pt(i * 1000, i as f64)).collect();
        let out = downsample(&pts);
        assert!(out.len() <= HISTORY_MAX_SERIES_POINTS);
        // 1000 -> step 2 -> 500 buckets averaging pairs (i, i+1)
        assert_eq!(out.len(), 500);
        assert!((out[0].up - 0.5).abs() < 1e-9);
        assert!((out[1].up - 2.5).abs() < 1e-9);
    }
}
