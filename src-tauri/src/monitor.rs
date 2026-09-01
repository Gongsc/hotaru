use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Emitter, Manager};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::models::{
    report_to_snapshot, normalize_base, now_ms, ws_url_of, ClientInfo, Envelope,
    MonitorSnapshot, NetFrame, NodeSnapshot, Report, Settings, WsPayload,
};
use crate::state::AppState;
use crate::tray;

const EVENT: &str = "monitor://update";
const NODES_REFRESH: Duration = Duration::from_secs(60);
const TRAY_MIN_INTERVAL: Duration = Duration::from_millis(1000);
const WS_READ_TIMEOUT: Duration = Duration::from_secs(15);
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(engine(app));
}

async fn engine(app: AppHandle) {
    let mut rx = app.state::<AppState>().config_epoch_tx.subscribe();
    loop {
        let settings = app.state::<AppState>().settings.read().clone().sanitized();
        if settings.backend_url.is_empty() {
            set_error(&app, "未配置后端地址，请在设置中填写");
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        }
        let client = build_client(&settings);
        match ws_session(&app, &client, &settings, &mut rx).await {
            Ok(()) => continue, // settings changed -> reconnect with new config
            Err(e) => set_error(&app, &format!("实时连接不可用（{e}），已回退 HTTP 轮询")),
        }
        match http_loop(&app, &client, &settings, &mut rx).await {
            Ok(()) => continue,
            Err(e) => set_error(&app, &e),
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
            _ = rx.changed() => {}
        }
    }
}

fn build_client(s: &Settings) -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(s.accept_invalid_certs)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// WebSocket live session (pull model: send "get", read one snapshot)
// ---------------------------------------------------------------------------

async fn ws_session(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let url = ws_url_of(&s.backend_url)?;
    let mut request = url
        .clone()
        .into_client_request()
        .map_err(|e| format!("构造 WS 请求失败: {e}"))?;
    {
        let headers = request.headers_mut();
        if !s.api_key.is_empty() {
            let value = HeaderValue::from_str(&format!("Bearer {}", s.api_key))
                .map_err(|_| "API Key 含非法字符".to_string())?;
            headers.insert("authorization", value);
        }
        let origin = HeaderValue::from_str(&origin_of_safe(&base))
            .map_err(|_| "后端地址含非法字符".to_string())?;
        headers.insert("origin", origin);
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("连接失败: {e}"))?;
    let (mut write, mut read) = ws.split();

    let mut nodes = fetch_nodes(client, &base, &s.api_key).await.unwrap_or_default();
    let mut last_nodes = Instant::now();
    let mut last_tray = Instant::now() - TRAY_MIN_INTERVAL;
    let mut interval = tokio::time::interval(Duration::from_secs(s.poll_interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    clear_error(app);
    log::info!("WS 已连接: {url}");

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            _ = interval.tick() => {
                if last_nodes.elapsed() >= NODES_REFRESH {
                    if let Ok(n) = fetch_nodes(client, &base, &s.api_key).await {
                        nodes = n;
                    }
                    last_nodes = Instant::now();
                }
                if write.send(Message::Text("get".into())).await.is_err() {
                    return Err("发送 WS 消息失败".into());
                }
                let msg = match tokio::time::timeout(WS_READ_TIMEOUT, read.next()).await {
                    Ok(Some(Ok(m))) => m,
                    Ok(Some(Err(e))) => return Err(format!("读取失败: {e}")),
                    Ok(None) => return Err("连接已被服务端关闭".into()),
                    Err(_) => return Err("读取超时".into()),
                };
                if let Message::Text(txt) = msg {
                    if let Some(snap) = parse_ws_text(&txt, &nodes) {
                        publish(app, snap, &mut last_tray);
                    }
                }
            }
        }
    }
}

pub fn parse_ws_text(txt: &str, nodes: &HashMap<String, ClientInfo>) -> Option<MonitorSnapshot> {
    let env: Envelope<WsPayload> = serde_json::from_str(txt).ok()?;
    let payload = env.data?;
    // 以已知节点列表为准:未出现在实时推送里的节点(如 HTTP 上报的
    // 路由器设备)按离线展示,而不是直接消失。
    let mut out: Vec<NodeSnapshot> = nodes
        .values()
        .map(|info| {
            let online = payload.online.iter().any(|o| o == &info.uuid);
            match payload.data.get(&info.uuid) {
                Some(rep) => report_to_snapshot(info, online, rep),
                None => report_to_snapshot(info, false, &Report::default()),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(MonitorSnapshot {
        backend_ok: true,
        error: None,
        nodes: out,
        last_update_ms: now_ms(),
    })
}

// ---------------------------------------------------------------------------
// HTTP polling fallback: /api/nodes + /api/recent/{uuid}
// ---------------------------------------------------------------------------

async fn http_loop(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let mut nodes = fetch_nodes(client, &base, &s.api_key).await?;
    let mut last_nodes = Instant::now();
    let mut last_tray = Instant::now() - TRAY_MIN_INTERVAL;
    log::info!("HTTP 轮询模式: {base}");
    clear_error(app);

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(s.poll_interval_secs.max(1))) => {
                if last_nodes.elapsed() >= NODES_REFRESH {
                    nodes = fetch_nodes(client, &base, &s.api_key).await?;
                    last_nodes = Instant::now();
                }
                let mut out = Vec::with_capacity(nodes.len());
                for (uuid, info) in &nodes {
                    let url = format!("{base}/api/recent/{uuid}");
                    let mut req = client.get(&url);
                    if !s.api_key.is_empty() {
                        req = req.bearer_auth(&s.api_key);
                    }
                    let snapshot = match req.send().await {
                        Ok(resp) if resp.status().is_success() => match resp.json::<Envelope<Vec<Report>>>().await {
                            Ok(env) => match env.data {
                                Some(reports) if !reports.is_empty() => {
                                    report_to_snapshot(info, true, reports.last().unwrap())
                                }
                                _ => offline_snapshot(info),
                            },
                            Err(_) => offline_snapshot(info),
                        },
                        _ => offline_snapshot(info),
                    };
                    out.push(snapshot);
                }
                out.sort_by(|a, b| a.name.cmp(&b.name));
                publish(
                    app,
                    MonitorSnapshot { backend_ok: true, error: None, nodes: out, last_update_ms: now_ms() },
                    &mut last_tray,
                );
            }
        }
    }
}

fn offline_snapshot(info: &ClientInfo) -> NodeSnapshot {
    report_to_snapshot(info, false, &Report::default())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn fetch_nodes(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<HashMap<String, ClientInfo>, String> {
    let url = format!("{base}/api/nodes");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("请求 /api/nodes 失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("后端返回 401：站点为私有模式，请在设置中填写 API Key".into());
    }
    if !status.is_success() {
        return Err(format!("请求 /api/nodes 失败: HTTP {status}"));
    }
    let env: Envelope<Vec<ClientInfo>> =
        serde_json::from_str(&text).map_err(|e| format!("解析 /api/nodes 失败: {e}"))?;
    if env.status != "success" {
        return Err(format!(
            "后端返回错误: {}{}",
            env.status,
            env.message.map(|m| format!("（{m}）")).unwrap_or_default()
        ));
    }
    let data = env.data.ok_or("响应缺少 data 字段")?;
    Ok(data
        .into_iter()
        .filter(|c| !c.uuid.is_empty())
        .map(|c| (c.uuid.clone(), c))
        .collect())
}

fn publish(app: &AppHandle, snap: MonitorSnapshot, last_tray: &mut Instant) {
    {
        let st = app.state::<AppState>();
        *st.snapshot.write() = snap.clone();
        let nodes = snap
            .nodes
            .iter()
            .map(|n| (n.uuid.clone(), n.net_up, n.net_down, n.online))
            .collect();
        st.net_history.push(NetFrame { t: snap.last_update_ms, nodes });
    }
    let _ = app.emit(EVENT, &snap);
    if last_tray.elapsed() >= TRAY_MIN_INTERVAL {
        *last_tray = Instant::now();
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || tray::apply(&app2));
    }
}

fn set_error(app: &AppHandle, msg: &str) {
    {
        let st = app.state::<AppState>();
        let mut snap = st.snapshot.write();
        snap.backend_ok = false;
        snap.error = Some(msg.to_string());
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || tray::apply(&app2));
}

fn clear_error(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        let st = app2.state::<AppState>();
        let mut snap = st.snapshot.write();
        snap.backend_ok = true;
        snap.error = None;
    });
}

/// Origin header for the WS handshake (bypasses no origin check failures on
/// public sites without an API key).
fn origin_of_safe(base: &str) -> String {
    crate::models::origin_of(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_parse_keeps_nodes_without_live_data() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "uuid-1".to_string(),
            ClientInfo { uuid: "uuid-1".into(), name: "A".into(), ..Default::default() },
        );
        nodes.insert(
            "uuid-2".to_string(),
            ClientInfo { uuid: "uuid-2".into(), name: "B".into(), ..Default::default() },
        );
        nodes.insert(
            "uuid-3".to_string(),
            ClientInfo { uuid: "uuid-3".into(), name: "C".into(), ..Default::default() },
        );
        let txt = r#"{"status":"success","data":{"data":{"uuid-1":{"cpu":{"usage":10}}},"online":["uuid-1"]}}"#;
        let snap = parse_ws_text(txt, &nodes).expect("parse ok");
        assert_eq!(snap.nodes.len(), 3);
        let a = snap.nodes.iter().find(|n| n.uuid == "uuid-1").unwrap();
        assert!(a.online);
        assert!((a.cpu_usage - 10.0).abs() < 1e-9);
        let c = snap.nodes.iter().find(|n| n.uuid == "uuid-3").unwrap();
        assert!(!c.online);
        assert_eq!(c.cpu_usage, 0.0);
    }
}
