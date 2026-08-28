use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Settings {
    pub backend_url: String,
    pub api_key: String,
    pub poll_interval_secs: u64,
    pub tray_mode: TrayMode,
    pub pinned_uuid: String,
    pub show_menu_bar_text: bool,
    pub cpu_warn_pct: f64,
    pub mem_warn_pct: f64,
    pub accept_invalid_certs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend_url: String::new(),
            api_key: String::new(),
            poll_interval_secs: 3,
            tray_mode: TrayMode::Aggregate,
            pinned_uuid: String::new(),
            show_menu_bar_text: true,
            cpu_warn_pct: 80.0,
            mem_warn_pct: 85.0,
            accept_invalid_certs: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayMode {
    Aggregate,
    Node,
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.backend_url = self.backend_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.poll_interval_secs = self.poll_interval_secs.clamp(1, 60);
        self.cpu_warn_pct = self.cpu_warn_pct.clamp(1.0, 100.0);
        self.mem_warn_pct = self.mem_warn_pct.clamp(1.0, 100.0);
        if self.pinned_uuid.trim().is_empty() {
            self.tray_mode = TrayMode::Aggregate;
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub uuid: String,
    pub name: String,
    pub online: bool,
    pub cpu_usage: f64,
    pub ram_used: u64,
    pub ram_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    /// B/s
    pub net_up: f64,
    /// B/s
    pub net_down: f64,
    pub total_up: u64,
    pub total_down: u64,
    pub tcp: u64,
    pub udp: u64,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub backend_ok: bool,
    pub error: Option<String>,
    pub nodes: Vec<NodeSnapshot>,
    pub last_update_ms: u64,
}

impl MonitorSnapshot {
    pub fn offline(error: &str) -> Self {
        Self {
            backend_ok: false,
            error: Some(error.to_string()),
            nodes: Vec::new(),
            last_update_ms: now_ms(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Aggregate {
    pub online: usize,
    pub total: usize,
    pub cpu: f64,
    pub mem_pct: f64,
    pub ram_used: u64,
    pub ram_total: u64,
    pub net_up: f64,
    pub net_down: f64,
    pub total_up: u64,
    pub total_down: u64,
}

/// Nodes covered by the current tray mode: all nodes, or the pinned one
/// (falling back to all when the pinned node vanished from the backend).
pub fn scoped_nodes<'a>(settings: &Settings, nodes: &'a [NodeSnapshot]) -> Vec<&'a NodeSnapshot> {
    match settings.tray_mode {
        TrayMode::Node => {
            let pinned: Vec<&NodeSnapshot> = nodes
                .iter()
                .filter(|n| n.uuid == settings.pinned_uuid)
                .collect();
            if pinned.is_empty() {
                nodes.iter().collect()
            } else {
                pinned
            }
        }
        TrayMode::Aggregate => nodes.iter().collect(),
    }
}

pub fn aggregate(nodes: &[&NodeSnapshot]) -> Aggregate {
    let total = nodes.len();
    let online: Vec<&&NodeSnapshot> = nodes.iter().filter(|n| n.online).collect();
    let count = online.len().max(1) as f64;
    let ram_used: u64 = online.iter().map(|n| n.ram_used).sum();
    let ram_total: u64 = online.iter().map(|n| n.ram_total).sum();
    Aggregate {
        online: online.len(),
        total,
        cpu: online.iter().map(|n| n.cpu_usage).sum::<f64>() / count,
        mem_pct: pct(ram_used, ram_total).unwrap_or(0.0),
        ram_used,
        ram_total,
        net_up: online.iter().map(|n| n.net_up).sum(),
        net_down: online.iter().map(|n| n.net_down).sum(),
        total_up: online.iter().map(|n| n.total_up).sum(),
        total_down: online.iter().map(|n| n.total_down).sum(),
    }
}

pub fn pct(used: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(used as f64 / total as f64 * 100.0)
    }
}

// ---------------------------------------------------------------------------
// Severity for the tray icon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Err,
    Down,
}

#[derive(Debug, Clone, Copy)]
pub struct IconState {
    pub severity: Severity,
    /// Worst metric (CPU or memory) in percent, when available.
    pub gauge: Option<f64>,
    pub badge: bool,
}

pub fn icon_state(settings: &Settings, snap: &MonitorSnapshot) -> IconState {
    if !snap.backend_ok {
        return IconState { severity: Severity::Down, gauge: None, badge: false };
    }
    let scope = scoped_nodes(settings, &snap.nodes);
    if scope.is_empty() {
        return IconState { severity: Severity::Down, gauge: None, badge: false };
    }
    let offline = scope.iter().filter(|n| !n.online).count();
    let online: Vec<&&NodeSnapshot> = scope.iter().filter(|n| n.online).collect();
    if online.is_empty() {
        return IconState { severity: Severity::Err, gauge: None, badge: false };
    }
    let mut gauge: Option<f64> = None;
    let mut severity = Severity::Ok;
    let err_threshold = |warn: f64| (warn + 15.0).min(100.0);
    for n in &online {
        for (value, warn) in [
            (n.cpu_usage, settings.cpu_warn_pct),
            (pct(n.ram_used, n.ram_total).unwrap_or(0.0), settings.mem_warn_pct),
        ] {
            let s = if value >= err_threshold(warn) {
                Severity::Err
            } else if value >= warn {
                Severity::Warn
            } else {
                Severity::Ok
            };
            if rank(s) > rank(severity) {
                severity = s;
            }
            if gauge.map_or(true, |g| value > g) {
                gauge = Some(value.clamp(0.0, 100.0));
            }
        }
    }
    let badge = match settings.tray_mode {
        TrayMode::Aggregate => offline > 0,
        TrayMode::Node => false,
    };
    IconState { severity, gauge, badge }
}

fn rank(s: Severity) -> u8 {
    match s {
        Severity::Ok => 0,
        Severity::Warn => 1,
        Severity::Err => 2,
        Severity::Down => 3,
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Normalize a backend base URL: trim, strip trailing slashes, default scheme.
pub fn normalize_base(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("后端地址为空".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed: url::Url =
        url::Url::parse(&with_scheme).map_err(|_| format!("无法解析后端地址: {trimmed}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(with_scheme),
        _ => Err(format!("不支持的协议: {}", parsed.scheme())),
    }
}

/// `https://a.b:8080/x` -> `https://a.b:8080` (used for the WS Origin header).
pub fn origin_of(base: &str) -> String {
    let parsed = url::Url::parse(base);
    if let Ok(u) = parsed {
        let port = match (u.scheme(), u.port()) {
            ("http", None) => Some(80),
            ("https", None) => Some(443),
            (_, p) => p,
        };
        let host = u.host_str().unwrap_or("");
        match port {
            Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
            None => format!("{}://{}", u.scheme(), host),
        }
    } else {
        base.to_string()
    }
}

pub fn ws_url_of(base: &str) -> Result<String, String> {
    let base = normalize_base(base)?;
    let ws = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    Ok(format!("{ws}/api/clients"))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Formatting helpers (shared by tray, tooltip and settings UI)
// ---------------------------------------------------------------------------

pub fn fmt_rate(bps: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bps < 0.0 {
        "0B/s".into()
    } else if bps < KB {
        format!("{bps:.0}B/s")
    } else if bps < MB {
        format!("{:.1}KB/s", bps / KB)
    } else if bps < GB {
        format!("{:.2}MB/s", bps / MB)
    } else {
        format!("{:.2}GB/s", bps / GB)
    }
}

pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    const TB: f64 = 1024.0 * GB;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes}B")
    } else if b < MB {
        format!("{:.1}KB", b / KB)
    } else if b < GB {
        format!("{:.2}MB", b / MB)
    } else if b < TB {
        format!("{:.2}GB", b / GB)
    } else {
        format!("{:.2}TB", b / TB)
    }
}

#[allow(dead_code)]
pub fn fmt_uptime(secs: u64) -> String {
    #[allow(clippy::manual_div_ceil)]
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}天{h}小时")
    } else if h > 0 {
        format!("{h}小时{m}分")
    } else {
        format!("{m}分钟")
    }
}

// ---------------------------------------------------------------------------
// Komari wire format (parsed payloads)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub status: String,
    #[serde(default)]
    pub data: Option<T>,
    #[serde(default)]
    pub message: Option<String>,
}

/// `GET /api/nodes` list item — only the fields we display; tolerant to
/// snake_case and camelCase spellings across Komari versions.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[allow(dead_code)]
pub struct ClientInfo {
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub region: String,
    #[serde(default, alias = "memTotal")]
    pub mem_total: u64,
    #[serde(default, alias = "osAlias", alias = "osName")]
    pub os: String,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            uuid: String::new(),
            name: String::new(),
            region: String::new(),
            mem_total: 0,
            os: String::new(),
        }
    }
}

/// WebSocket `{"status":"success","data":{"online":[...],"data":{...}}}`.
#[derive(Debug, Deserialize, Default)]
pub struct WsPayload {
    #[serde(default)]
    pub online: Vec<String>,
    #[serde(default)]
    pub data: std::collections::HashMap<String, Report>,
}

/// Live report (`/api/recent/{uuid}` item and `/api/clients` WS value).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Report {
    pub cpu: Option<Usage>,
    pub ram: Option<Mem>,
    pub swap: Option<Mem>,
    pub disk: Option<Mem>,
    pub network: Option<NetStat>,
    pub connections: Option<Conns>,
    #[serde(alias = "uptime")]
    pub uptime: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Usage {
    pub usage: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Mem {
    pub total: Option<u64>,
    pub used: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct NetStat {
    pub up: Option<f64>,
    pub down: Option<f64>,
    #[serde(default, alias = "totalUp")]
    pub total_up: Option<u64>,
    #[serde(default, alias = "totalDown")]
    pub total_down: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Conns {
    pub tcp: Option<u64>,
    pub udp: Option<u64>,
}

pub fn report_to_snapshot(uuid: &str, name: &str, online: bool, r: &Report) -> NodeSnapshot {
    NodeSnapshot {
        uuid: uuid.to_string(),
        name: if name.is_empty() {
            format!("节点 {}", &uuid[..uuid.len().min(8)])
        } else {
            name.to_string()
        },
        online,
        cpu_usage: r.cpu.as_ref().and_then(|c| c.usage).unwrap_or(0.0),
        ram_used: r.ram.as_ref().and_then(|m| m.used).unwrap_or(0),
        ram_total: r.ram.as_ref().and_then(|m| m.total).unwrap_or(0),
        swap_used: r.swap.as_ref().and_then(|m| m.used).unwrap_or(0),
        swap_total: r.swap.as_ref().and_then(|m| m.total).unwrap_or(0),
        disk_used: r.disk.as_ref().and_then(|m| m.used).unwrap_or(0),
        disk_total: r.disk.as_ref().and_then(|m| m.total).unwrap_or(0),
        net_up: r.network.as_ref().and_then(|n| n.up).unwrap_or(0.0),
        net_down: r.network.as_ref().and_then(|n| n.down).unwrap_or(0.0),
        total_up: r.network.as_ref().and_then(|n| n.total_up).unwrap_or(0),
        total_down: r.network.as_ref().and_then(|n| n.total_down).unwrap_or(0),
        tcp: r.connections.as_ref().and_then(|c| c.tcp).unwrap_or(0),
        udp: r.connections.as_ref().and_then(|c| c.udp).unwrap_or(0),
        uptime_secs: r.uptime.unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_variants() {
        assert_eq!(normalize_base(" https://a.com/ ").unwrap(), "https://a.com");
        assert_eq!(normalize_base("a.com").unwrap(), "https://a.com");
        assert_eq!(normalize_base("http://a.com:8080//").unwrap(), "http://a.com:8080");
        assert!(normalize_base("").is_err());
        assert!(normalize_base("ftp://a.com").is_err());
    }

    #[test]
    fn origin_variants() {
        assert_eq!(origin_of("https://a.com"), "https://a.com:443");
        assert_eq!(origin_of("https://a.com:8443/x"), "https://a.com:8443");
        assert_eq!(origin_of("http://a.com"), "http://a.com:80");
        assert_eq!(origin_of("http://a.com:9000"), "http://a.com:9000");
    }

    #[test]
    fn ws_url_variants() {
        assert_eq!(ws_url_of("https://a.com").unwrap(), "wss://a.com/api/clients");
        assert_eq!(ws_url_of("http://a.com").unwrap(), "ws://a.com/api/clients");
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_rate(512.0), "512B/s");
        assert_eq!(fmt_rate(2048.0), "2.0KB/s");
        assert_eq!(fmt_rate(3.4 * 1024.0 * 1024.0), "3.40MB/s");
        assert_eq!(fmt_bytes(0), "0B");
        assert_eq!(fmt_bytes(5 * 1024 * 1024 * 1024), "5.00GB");
        assert_eq!(fmt_uptime(3600 * 27), "1天3小时");
        assert_eq!(fmt_uptime(300), "5分钟");
    }

    fn sample_ws_json() -> String {
        r#"{
          "status": "success",
          "data": {
            "online": ["uuid-1"],
            "data": {
              "uuid-1": {
                "cpu": {"usage": 42.5},
                "ram": {"total": 8589934592, "used": 4294967296},
                "swap": {"total": 1073741824, "used": 0},
                "disk": {"total": 107374182400, "used": 53687091200},
                "network": {"up": 128.0, "down": 3567155.2, "totalUp": 1024, "totalDown": 2048},
                "connections": {"tcp": 120, "udp": 8},
                "uptime": 98765
              },
              "uuid-2": {
                "cpu": {"usage": 91.0},
                "ram": {"total": 4294967296, "used": 2147483648}
              }
            }
          }
        }"#
        .to_string()
    }

    #[test]
    fn parse_ws_payload() {
        let mut names = std::collections::HashMap::new();
        names.insert("uuid-1".to_string(), "node-1".to_string());
        let env: Envelope<WsPayload> = serde_json::from_str(&sample_ws_json()).unwrap();
        let payload = env.data.unwrap();
        assert_eq!(payload.online, vec!["uuid-1".to_string()]);
        assert_eq!(payload.data.len(), 2);
        let rep = payload.data.get("uuid-1").unwrap();
        let snap = report_to_snapshot("uuid-1", names.get("uuid-1").unwrap(), true, rep);
        assert_eq!(snap.name, "node-1");
        assert!((snap.cpu_usage - 42.5).abs() < 1e-9);
        assert_eq!(snap.ram_used, 4294967296);
        assert!((snap.net_down - 3567155.2).abs() < 1e-9);
        assert_eq!(snap.tcp, 120);
        // unknown node gets a fallback name
        let rep2 = payload.data.get("uuid-2").unwrap();
        let snap2 = report_to_snapshot("uuid-2", "", false, rep2);
        assert_eq!(snap2.name, "节点 uuid-2");
        assert_eq!(snap2.uptime_secs, 0);
    }

    #[test]
    fn aggregate_math() {
        let mut names = std::collections::HashMap::new();
        names.insert("uuid-1".to_string(), "node-1".to_string());
        let env: Envelope<WsPayload> = serde_json::from_str(&sample_ws_json()).unwrap();
        let payload = env.data.unwrap();
        let nodes: Vec<NodeSnapshot> = payload
            .data
            .iter()
            .map(|(uuid, rep)| {
                let online = payload.online.iter().any(|o| o == uuid);
                report_to_snapshot(uuid, names.get(uuid).map(|s| s.as_str()).unwrap_or(""), online, rep)
            })
            .collect();
        let agg = aggregate(&nodes.iter().collect::<Vec<_>>());
        assert_eq!(agg.total, 2);
        assert_eq!(agg.online, 1);
        assert!((agg.cpu - 42.5).abs() < 1e-9);
        assert!((agg.mem_pct - 50.0).abs() < 1e-9);
        assert!((agg.net_down - 3567155.2).abs() < 1e-9);
        assert_eq!(agg.ram_total, 8589934592);
    }

    #[test]
    fn icon_severity_rules() {
        let mut s = Settings::default();
        s.cpu_warn_pct = 80.0;
        s.mem_warn_pct = 85.0;
        let snap = MonitorSnapshot {
            backend_ok: false,
            error: Some("x".into()),
            nodes: vec![],
            last_update_ms: 0,
        };
        assert_eq!(icon_state(&s, &snap).severity, Severity::Down);

        let snap = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![NodeSnapshot {
                cpu_usage: 42.0,
                ram_used: 4294967296,
                ram_total: 8589934592,
                online: true,
                ..NodeSnapshot::test_default()
            }],
            last_update_ms: 0,
        };
        let st = icon_state(&s, &snap);
        assert_eq!(st.severity, Severity::Ok);
        assert!((st.gauge.unwrap() - 50.0).abs() < 1e-9);
        assert!(!st.badge);

        // offline sibling triggers badge in aggregate mode
        let mut offline = NodeSnapshot::test_default();
        offline.online = false;
        let snap2 = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![snap.nodes[0].clone(), offline],
            last_update_ms: 0,
        };
        assert!(icon_state(&s, &snap2).badge);

        // cpu above warn -> Warn
        let mut high = snap.nodes[0].clone();
        high.cpu_usage = 88.0;
        let snap3 = MonitorSnapshot { nodes: vec![high], ..snap };
        assert_eq!(icon_state(&s, &snap3).severity, Severity::Warn);

        // node mode without badge
        let mut s_node = s.clone();
        s_node.tray_mode = TrayMode::Node;
        s_node.pinned_uuid = "pinned".into();
        let mut pinned = NodeSnapshot::test_default();
        pinned.uuid = "pinned".into();
        pinned.online = true;
        let other = NodeSnapshot {
            uuid: "other".into(),
            online: false,
            ..NodeSnapshot::test_default()
        };
        let snap4 = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![pinned, other],
            last_update_ms: 0,
        };
        assert!(!icon_state(&s_node, &snap4).badge);
        assert_eq!(icon_state(&s_node, &snap4).severity, Severity::Ok);
    }

    #[test]
    fn scoped_nodes_fallback() {
        let mut s = Settings::default();
        s.tray_mode = TrayMode::Node;
        s.pinned_uuid = "missing".into();
        let nodes = vec![NodeSnapshot {
            uuid: "a".into(),
            ..NodeSnapshot::test_default()
        }];
        // pinned node vanished -> falls back to all nodes
        assert_eq!(scoped_nodes(&s, &nodes).len(), 1);
    }

    impl NodeSnapshot {
        fn test_default() -> Self {
            Self {
                uuid: "u".into(),
                name: "n".into(),
                online: true,
                cpu_usage: 0.0,
                ram_used: 0,
                ram_total: 0,
                swap_used: 0,
                swap_total: 0,
                disk_used: 0,
                disk_total: 0,
                net_up: 0.0,
                net_down: 0.0,
                total_up: 0,
                total_down: 0,
                tcp: 0,
                udp: 0,
                uptime_secs: 0,
            }
        }
    }
}
