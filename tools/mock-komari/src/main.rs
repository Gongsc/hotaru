// Minimal mock Komari backend for end-to-end testing (HTTP only; the app
// falls back from WebSocket to HTTP polling, which is what we exercise).
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn nodes_json() -> String {
    format!(
        r#"{{"status":"success","data":[
            {{"uuid":"11111111-1111-1111-1111-111111111111","name":"测试节点A","region":"CN","mem_total":8589934592,"os":"Debian 12"}},
            {{"uuid":"22222222-2222-2222-2222-222222222222","name":"测试节点B","region":"US","mem_total":4294967296,"os":"Ubuntu 24.04"}}
        ]}}"#
    )
}

fn report(cpu: f64, ram_used: u64, ram_total: u64, up: f64, down: f64) -> String {
    format!(
        r#"{{"cpu":{{"usage":{cpu}}},"ram":{{"total":{ram_total},"used":{ram_used}}},"swap":{{"total":1073741824,"used":0}},"disk":{{"total":107374182400,"used":53687091200}},"network":{{"up":{up},"down":{down},"totalUp":123456789,"totalDown":987654321}},"connections":{{"tcp":120,"udp":8}},"uptime":98765,"updated_at":{}}}"#,
        now_ms()
    )
}

fn recent_json(uuid: &str) -> String {
    // A: healthy (cpu 32%, mem 50%), B: hot (cpu 88% -> warn at default 80%)
    let body = if uuid.starts_with("1111") {
        report(32.0, 4294967296, 8589934592, 2048.0, 3355443.2)
    } else {
        report(88.0, 2147483648, 4294967296, 8192.0, 12582912.0)
    };
    format!(r#"{{"status":"success","data":[{body}]}}"#)
}

fn handle(stream: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let mut req = String::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                req.push_str(&String::from_utf8_lossy(&buf[..n]));
                if req.contains("\r\n\r\n") || req.len() > 16384 {
                    break;
                }
            }
        }
    }
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let (status, body) = match path.as_str() {
        "/ping" => ("200 OK", "pong".to_string()),
        "/api/version" => ("200 OK", r#"{"version":"0.0.0-mock","hash":"deadbeef"}"#.to_string()),
        "/api/nodes" => ("200 OK", nodes_json()),
        p if p.starts_with("/api/recent/") => {
            let uuid = p.trim_start_matches("/api/recent/");
            ("200 OK", recent_json(uuid))
        }
        _ => ("404 Not Found", r#"{"status":"error","message":"not found"}"#.to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:25774").expect("bind 25774");
    eprintln!("mock komari listening on http://127.0.0.1:25774");
    let mut cache: HashMap<String, Vec<u8>> = HashMap::new();
    for stream in listener.incoming() {
        let mut s = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        // reject WebSocket upgrades so the app exercises its HTTP fallback
        if cache.insert("handshake".into(), vec![]).is_none() {}
        handle(&mut s);
    }
}
