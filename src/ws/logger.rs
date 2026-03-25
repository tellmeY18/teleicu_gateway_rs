use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde_json::json;
use std::time::Duration;
use sysinfo::System;

/// WS /logger — stream system resource metrics every 2 seconds.
/// No authentication (internal use only).
pub async fn ws_logger(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_logger)
}

async fn handle_logger(mut socket: WebSocket) {
    let mut sys = System::new_all();
    let boot_time = System::boot_time();

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        sys.refresh_all();

        let cpu_usage = sys.global_cpu_info().cpu_usage();
        let used_memory = sys.used_memory();
        let total_memory = sys.total_memory();
        let memory_pct = if total_memory > 0 {
            (used_memory as f64 / total_memory as f64) * 100.0
        } else {
            0.0
        };

        let uptime_ms = {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            (now_secs - boot_time) * 1000
        };

        let load = System::load_average();

        let payload = json!({
            "type": "RESOURCE",
            "cpu": format!("{:.2}", cpu_usage),
            "memory": format!("{:.2}", memory_pct),
            "uptime": uptime_ms,
            "load": format!("{:.2}", load.five),
        });

        let json_str = payload.to_string();
        if socket.send(Message::Text(json_str.into())).await.is_err() {
            // Client disconnected
            break;
        }
    }
}
