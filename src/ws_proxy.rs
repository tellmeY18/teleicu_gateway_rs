//! WebSocket proxy module for tunneling video streams to RTSPtoWeb.
//!
//! This module handles WebSocket upgrades and establishes bidirectional tunnels
//! between clients and the upstream RTSPtoWeb service for MSE video streaming.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMessage};

/// Handle WebSocket proxy request by upgrading and tunneling to RTSPtoWeb.
///
/// This function:
/// 1. Extracts the WebSocket upgrade from the request
/// 2. Builds the upstream WebSocket URL
/// 3. Upgrades the client connection
/// 4. Establishes bidirectional tunnel to RTSPtoWeb
pub async fn handle_websocket_proxy(
    ws_upgrade: WebSocketUpgrade,
    path: String,
    query: String,
    rtsptoweb_base: String,
) -> Result<Response, crate::error::AppError> {
    // Convert http:// to ws:// for WebSocket connection
    let ws_url = format!(
        "{}{}{}",
        rtsptoweb_base
            .trim_end_matches('/')
            .replace("http://", "ws://")
            .replace("https://", "wss://"),
        path,
        query
    );

    tracing::info!(
        target: "teleicu_gateway::proxy",
        "🎥 WebSocket tunnel request: {}",
        ws_url
    );

    // Upgrade the client connection and establish tunnel
    Ok(ws_upgrade.on_upgrade(move |client_socket| async move {
        let start = std::time::Instant::now();

        match tunnel_websocket(client_socket, ws_url.clone()).await {
            Ok(_) => {
                tracing::info!(
                    target: "teleicu_gateway::proxy",
                    "✅ WebSocket tunnel closed normally: {} (duration: {:?})",
                    ws_url,
                    start.elapsed()
                );
            }
            Err(e) => {
                tracing::error!(
                    target: "teleicu_gateway::proxy",
                    "❌ WebSocket tunnel error: {} - {}",
                    ws_url,
                    e
                );
            }
        }
    }))
}

/// Establish bidirectional tunnel between client WebSocket and upstream RTSPtoWeb WebSocket.
///
/// Creates two async tasks:
/// - Task 1: Forward frames from client → upstream
/// - Task 2: Forward frames from upstream → client
///
/// Runs until either side closes the connection.
async fn tunnel_websocket(
    client_socket: WebSocket,
    upstream_url: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Connect to upstream RTSPtoWeb WebSocket
    let (upstream_ws, _) = connect_async(&upstream_url).await.map_err(|e| {
        tracing::error!(
            target: "teleicu_gateway::proxy",
            "Failed to connect to upstream WebSocket {}: {}",
            upstream_url,
            e
        );
        e
    })?;

    tracing::debug!(
        target: "teleicu_gateway::proxy",
        "Connected to upstream WebSocket: {}",
        upstream_url
    );

    // Split both WebSocket connections for bidirectional communication
    let (client_tx, mut client_rx) = client_socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream_ws.split();

    // Wrap client_tx in Arc<Mutex> for sharing between tasks
    let client_tx = std::sync::Arc::new(tokio::sync::Mutex::new(client_tx));
    let client_tx_clone = client_tx.clone();

    // Task 1: Forward client frames → upstream
    let client_to_upstream = tokio::spawn(async move {
        let mut frame_count = 0u64;

        while let Some(msg_result) = client_rx.next().await {
            match msg_result {
                Ok(msg) => {
                    // Convert axum WebSocket message to tungstenite message
                    let upstream_msg = match msg {
                        Message::Text(t) => TungsteniteMessage::Text(t),
                        Message::Binary(b) => TungsteniteMessage::Binary(b),
                        Message::Ping(p) => TungsteniteMessage::Ping(p),
                        Message::Pong(p) => TungsteniteMessage::Pong(p),
                        Message::Close(_) => {
                            tracing::debug!(
                                target: "teleicu_gateway::proxy",
                                "Client sent close frame"
                            );
                            break;
                        }
                    };

                    if upstream_tx.send(upstream_msg).await.is_err() {
                        tracing::debug!(
                            target: "teleicu_gateway::proxy",
                            "Upstream connection closed while sending"
                        );
                        break;
                    }

                    frame_count += 1;
                }
                Err(e) => {
                    tracing::debug!(
                        target: "teleicu_gateway::proxy",
                        "Client socket error: {}",
                        e
                    );
                    break;
                }
            }
        }

        tracing::debug!(
            target: "teleicu_gateway::proxy",
            "⬆️  Client → Upstream: {} frames forwarded",
            frame_count
        );
    });

    // Task 2: Forward upstream frames → client
    let upstream_to_client = tokio::spawn(async move {
        let mut frame_count = 0u64;

        while let Some(msg_result) = upstream_rx.next().await {
            match msg_result {
                Ok(msg) => {
                    // Convert tungstenite message to axum WebSocket message
                    let client_msg = match msg {
                        TungsteniteMessage::Text(t) => Message::Text(t),
                        TungsteniteMessage::Binary(b) => Message::Binary(b),
                        TungsteniteMessage::Ping(p) => Message::Ping(p),
                        TungsteniteMessage::Pong(p) => Message::Pong(p),
                        TungsteniteMessage::Close(_) => {
                            tracing::debug!(
                                target: "teleicu_gateway::proxy",
                                "Upstream sent close frame"
                            );
                            break;
                        }
                        TungsteniteMessage::Frame(_) => continue, // Raw frames, skip
                    };

                    let mut tx = client_tx_clone.lock().await;
                    if tx.send(client_msg).await.is_err() {
                        tracing::debug!(
                            target: "teleicu_gateway::proxy",
                            "Client connection closed while sending"
                        );
                        break;
                    }

                    frame_count += 1;
                }
                Err(e) => {
                    tracing::debug!(
                        target: "teleicu_gateway::proxy",
                        "Upstream socket error: {}",
                        e
                    );
                    break;
                }
            }
        }

        tracing::debug!(
            target: "teleicu_gateway::proxy",
            "⬇️  Upstream → Client: {} frames forwarded",
            frame_count
        );
    });

    // Wait for either direction to complete (disconnect)
    tokio::select! {
        _ = client_to_upstream => {
            tracing::debug!(
                target: "teleicu_gateway::proxy",
                "Client-to-upstream task completed"
            );
        }
        _ = upstream_to_client => {
            tracing::debug!(
                target: "teleicu_gateway::proxy",
                "Upstream-to-client task completed"
            );
        }
    }

    Ok(())
}
