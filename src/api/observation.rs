use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde_json::{json, Value};
use std::net::SocketAddr;

use crate::auth::inbound::CareAuth;
use crate::error::AppError;
use crate::observations::types::Observation;
use crate::state::AppState;

/// POST /update_observations — ingest observations from bedside monitors.
/// Security: only accept from loopback or LAN addresses.
pub async fn update_observations(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(observations): Json<Vec<Observation>>,
) -> Result<StatusCode, AppError> {
    // Security: reject non-loopback, non-private IPs
    let ip = addr.ip();
    let is_local = ip.is_loopback()
        || match ip {
            std::net::IpAddr::V4(v4) => v4.is_private(),
            std::net::IpAddr::V6(_) => ip.is_loopback(),
        };

    if !is_local {
        tracing::warn!("Rejected /update_observations from non-local IP: {ip}");
        return Err(AppError::Unauthorized);
    }

    tracing::debug!("Ingesting {} observations from {ip}", observations.len());
    state.obs_store.ingest(observations);
    Ok(StatusCode::OK)
}

/// GET /devices/status — return current device statuses.
/// Requires Care_Bearer auth.
pub async fn device_status(
    _auth: CareAuth,
    State(state): State<AppState>,
) -> Json<Value> {
    let statuses = state.obs_store.get_device_statuses();
    Json(json!({
        "time": Utc::now().to_rfc3339(),
        "status": statuses
    }))
}
