use axum::extract::{Query, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::inbound::CareAuth;
use crate::error::AppError;
use crate::onvif::client::OnvifClient;
use crate::state::AppState;

/// Query parameters for GET camera operations.
#[derive(Debug, Deserialize)]
pub struct CameraParams {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

/// POST body for move operations.
#[derive(Debug, Deserialize)]
pub struct CameraMoveRequest {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

/// POST body for preset operations.
#[derive(Debug, Deserialize)]
pub struct CameraPresetRequest {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub preset: Option<i32>,
    #[serde(alias = "presetName")]
    pub preset_name: Option<String>,
}

/// Helper to build an OnvifClient from credentials.
fn make_client(state: &AppState, hostname: &str, port: u16, username: &str, password: &str) -> OnvifClient {
    OnvifClient::new(state.http.clone(), hostname, port, username, password)
}

/// GET /cameras/presets — list presets for a camera.
pub async fn get_presets(
    _auth: CareAuth,
    State(state): State<AppState>,
    Query(params): Query<CameraParams>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 GET /cameras/presets - hostname: {}, port: {}, user: {}",
        params.hostname,
        params.port,
        params.username
    );
    let client = make_client(&state, &params.hostname, params.port, &params.username, &params.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    let presets = client.get_presets(profile_token).await?;

    // Return as { name: index } map (original API contract)
    let preset_map: serde_json::Map<String, Value> = presets
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), json!(i)))
        .collect();

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Presets retrieved for {} - count: {}",
        params.hostname,
        presets.len()
    );

    Ok(Json(json!({ "presets": preset_map })))
}

/// GET /cameras/status — get PTZ status for a camera.
pub async fn get_camera_status(
    _auth: CareAuth,
    State(state): State<AppState>,
    Query(params): Query<CameraParams>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 GET /cameras/status - hostname: {}, port: {}",
        params.hostname,
        params.port
    );
    let client = make_client(&state, &params.hostname, params.port, &params.username, &params.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    let status = client.get_status(profile_token).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Camera status retrieved for {}",
        params.hostname
    );

    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}

/// POST /cameras/absoluteMove — absolute PTZ move with camera lock.
pub async fn absolute_move(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 POST /cameras/absoluteMove - hostname: {}, x: {}, y: {}, zoom: {}",
        req.hostname,
        req.x,
        req.y,
        req.zoom
    );

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Acquiring camera lock for {}",
        req.hostname
    );

    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Camera lock acquired for {}",
        req.hostname
    );

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.absolute_move(profile_token, req.x, req.y, req.zoom).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Waiting for camera {} to reach idle state",
        req.hostname
    );

    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Absolute move completed for {} to position ({}, {}, {})",
        req.hostname,
        req.x,
        req.y,
        req.zoom
    );

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/relativeMove — relative PTZ move with camera lock.
pub async fn relative_move(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 POST /cameras/relativeMove - hostname: {}, dx: {}, dy: {}, dzoom: {}",
        req.hostname,
        req.x,
        req.y,
        req.zoom
    );

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Acquiring camera lock for {}",
        req.hostname
    );

    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Camera lock acquired for {}",
        req.hostname
    );

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.relative_move(profile_token, req.x, req.y, req.zoom).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Waiting for camera {} to reach idle state",
        req.hostname
    );

    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Relative move completed for {} by delta ({}, {}, {})",
        req.hostname,
        req.x,
        req.y,
        req.zoom
    );

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/snapshotAtLocation — move to position, then get snapshot URI.
pub async fn snapshot_at_location(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 POST /cameras/snapshotAtLocation - hostname: {}, x: {}, y: {}, zoom: {}",
        req.hostname,
        req.x,
        req.y,
        req.zoom
    );

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Acquiring camera lock for {}",
        req.hostname
    );

    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Camera lock acquired for {}",
        req.hostname
    );

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.absolute_move(profile_token, req.x, req.y, req.zoom).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Waiting for camera {} to reach idle state before snapshot",
        req.hostname
    );

    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    let uri = client.get_snapshot_uri(profile_token).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Snapshot URI retrieved for {} at position ({}, {}, {}): {}",
        req.hostname,
        req.x,
        req.y,
        req.zoom,
        uri
    );

    Ok(Json(json!({ "status": "ok", "uri": uri })))
}

/// POST /cameras/gotoPreset — go to a preset by numeric index.
pub async fn goto_preset(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraPresetRequest>,
) -> Result<Json<Value>, AppError> {
    let preset_index = req.preset.ok_or_else(|| {
        AppError::Onvif("preset index is required".into())
    })?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 POST /cameras/gotoPreset - hostname: {}, preset_index: {}",
        req.hostname,
        preset_index
    );

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Acquiring camera lock for {}",
        req.hostname
    );

    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Camera lock acquired for {}",
        req.hostname
    );

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    let presets = client.get_presets(profile_token).await?;
    let preset = presets.get(preset_index as usize).ok_or(AppError::NotFound)?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Moving camera {} to preset: {} (token: {})",
        req.hostname,
        preset.name,
        preset.token
    );

    client.goto_preset(profile_token, &preset.token).await?;

    tracing::debug!(
        target: "teleicu_gateway::camera",
        "Waiting for camera {} to reach idle state",
        req.hostname
    );

    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Camera {} moved to preset #{} ({})",
        req.hostname,
        preset_index,
        preset.name
    );

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/set_preset — create a new preset at the current position.
pub async fn set_preset(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraPresetRequest>,
) -> Result<Json<Value>, AppError> {
    let preset_name = req.preset_name.as_deref().ok_or_else(|| {
        AppError::Onvif("preset_name is required".into())
    })?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "📷 POST /cameras/set_preset - hostname: {}, preset_name: {}",
        req.hostname,
        preset_name
    );

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.set_preset(profile_token, preset_name).await?;

    tracing::info!(
        target: "teleicu_gateway::camera",
        "✅ Preset '{}' saved for camera {}",
        preset_name,
        req.hostname
    );

    Ok(Json(json!({ "status": "ok" })))
}

/// GET /cameras/status (all cameras) — return device statuses from observation store.
pub async fn cameras_status_all(
    _auth: CareAuth,
    State(state): State<AppState>,
) -> Json<Value> {
    tracing::debug!(
        target: "teleicu_gateway::camera",
        "📷 GET /cameras/status (all) - retrieving device statuses"
    );

    let statuses = state.obs_store.get_device_statuses();
    Json(json!({
        "time": Utc::now().to_rfc3339(),
        "status": statuses
    }))
}
