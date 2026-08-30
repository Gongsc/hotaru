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
        for (uuid, up, down, online) in &f.nodes {
            nodes.entry(uuid.clone()).or_default().push(NetPoint {
                t: f.t,
                up: *up,
                down: *down,
                online: *online,
            });
        }
    }

    NetHistoryPayload {
        aggregate: downsample(&frames.iter().map(|f| NetPoint {
            t: f.t,
            up: f.nodes.iter().map(|(_, up, _, _)| up).sum(),
            down: f.nodes.iter().map(|(_, _, down, _)| down).sum(),
            online: true,
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
                online: chunk.iter().any(|p| p.online),
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

#[derive(Serialize)]
pub struct PingPoint {
    /// Epoch milliseconds.
    pub t: u64,
    /// Latency in ms; negative means the probe was lost.
    pub v: f64,
}

/// Proxy the backend's ping-monitor records for one node, so the webview
/// never has to talk cross-origin to the backend.
#[tauri::command]
pub async fn get_ping_records(
    state: State<'_, AppState>,
    uuid: String,
    hours: u64,
) -> Result<Vec<PingPoint>, String> {
    let s = state.settings.read().clone().sanitized();
    let base = normalize_base(&s.backend_url)?;
    let hours = hours.clamp(1, 24);
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(s.accept_invalid_certs)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(format!("{base}/api/records/ping?uuid={uuid}&hours={hours}"));
    if !s.api_key.is_empty() {
        req = req.bearer_auth(&s.api_key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(records) = body.pointer("/data/records").and_then(|v| v.as_array()) {
        for r in records {
            let v = r.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let time = r.get("time").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(t) = parse_rfc3339_ms(time) {
                out.push(PingPoint { t, v });
            }
        }
    }
    out.sort_by_key(|p| p.t);
    Ok(out)
}

/// Minimal RFC3339 UTC parser ("2026-08-29T16:50:00Z", fractional seconds
/// allowed but ignored) — avoids pulling a full date-time crate.
fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let h = num(11, 13)?;
    let mi = num(14, 16)?;
    let sec = num(17, 19)?;
    let days = days_from_civil(y, mo, d);
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) * 1000) as u64)
}

/// Howard Hinnant's days_from_civil: days since 1970-01-01 for a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: u64, up: f64) -> NetPoint {
        NetPoint { t, up, down: up, online: true }
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

    #[test]
    fn rfc3339_parse_epoch() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("1970-01-02T00:00:00Z"), Some(86_400_000));
        // 2024-02-29 (leap day) 00:00 UTC
        assert_eq!(parse_rfc3339_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        // fractional seconds ignored
        assert_eq!(
            parse_rfc3339_ms("2024-02-29T00:00:00.123456Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(parse_rfc3339_ms("not-a-date"), None);
    }
}
