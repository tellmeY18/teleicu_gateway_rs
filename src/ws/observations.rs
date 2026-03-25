use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;

use crate::state::AppState;

/// WS /observations/<ip_address> — stream observations for a device.
///
/// Authentication: token is passed in `Sec-WebSocket-Protocol` header as "Token, <jwt>".
/// Validated against the gateway's own public key.
pub async fn ws_observations(
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    Path(ip_address): Path<String>,
    State(state): State<AppState>,
) -> Response {
    // Extract token from Sec-WebSocket-Protocol header
    let token: Option<String> = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            // Format: "Token, <jwt>"
            s.strip_prefix("Token, ")
                .or_else(|| s.strip_prefix("Token,").map(|t| t.trim()))
                .map(|t| t.to_string())
        });

    let token_str = match token {
        Some(t) => t,
        None => {
            return ws.on_upgrade(|socket| async move {
                let _ = socket.close().await;
            });
        }
    };

    if state.own_keypair.verify_jwt(&token_str).is_err() {
        return ws.on_upgrade(|socket| async move {
            let _ = socket.close().await;
        });
    }

    ws.protocols(["Token"])
        .on_upgrade(move |socket| handle_observations(socket, ip_address, state))
}

async fn handle_observations(mut socket: WebSocket, ip_address: String, state: AppState) {
    let mut rx = state.obs_store.subscribe(&ip_address);

    loop {
        match rx.recv().await {
            Ok(observations) => {
                let json = match serde_json::to_string(&observations) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!("Failed to serialize observations: {e}");
                        continue;
                    }
                };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("WebSocket client lagged, skipped {n} messages for {ip_address}");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break;
            }
        }
    }
}
