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
    tracing::trace!("get_presets for {}", params.hostname);
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

    Ok(Json(json!({ "presets": preset_map })))
}

/// GET /cameras/status — get PTZ status for a camera.
pub async fn get_camera_status(
    _auth: CareAuth,
    State(state): State<AppState>,
    Query(params): Query<CameraParams>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("get_camera_status for {}", params.hostname);
    let client = make_client(&state, &params.hostname, params.port, &params.username, &params.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    let status = client.get_status(profile_token).await?;
    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}

/// POST /cameras/absoluteMove — absolute PTZ move with camera lock.
pub async fn absolute_move(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("absolute_move for {}", req.hostname);
    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.absolute_move(profile_token, req.x, req.y, req.zoom).await?;
    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/relativeMove — relative PTZ move with camera lock.
pub async fn relative_move(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("relative_move for {}", req.hostname);
    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.relative_move(profile_token, req.x, req.y, req.zoom).await?;
    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/snapshotAtLocation — move to position, then get snapshot URI.
pub async fn snapshot_at_location(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraMoveRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("snapshot_at_location for {}", req.hostname);
    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.absolute_move(profile_token, req.x, req.y, req.zoom).await?;
    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    let uri = client.get_snapshot_uri(profile_token).await?;
    Ok(Json(json!({ "status": "ok", "uri": uri })))
}

/// POST /cameras/gotoPreset — go to a preset by numeric index.
pub async fn goto_preset(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraPresetRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("goto_preset for {}", req.hostname);
    let _lock = state.camera_locks.try_lock(&req.hostname).await?;

    let preset_index = req.preset.ok_or_else(|| {
        AppError::Onvif("preset index is required".into())
    })?;

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    let presets = client.get_presets(profile_token).await?;
    let preset = presets.get(preset_index as usize).ok_or(AppError::NotFound)?;

    client.goto_preset(profile_token, &preset.token).await?;
    client.wait_for_idle(profile_token, state.settings.camera_lock_timeout_secs).await?;

    Ok(Json(json!({ "status": "ok" })))
}

/// POST /cameras/set_preset — create a new preset at the current position.
pub async fn set_preset(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<CameraPresetRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::trace!("set_preset for {}", req.hostname);

    let preset_name = req.preset_name.as_deref().ok_or_else(|| {
        AppError::Onvif("preset_name is required".into())
    })?;

    let client = make_client(&state, &req.hostname, req.port, &req.username, &req.password);
    let profiles = client.get_profiles().await?;
    let profile_token = profiles
        .first()
        .map(|p| p.token.as_str())
        .ok_or_else(|| AppError::Onvif("no profiles found".into()))?;

    client.set_preset(profile_token, preset_name).await?;
    Ok(Json(json!({ "status": "ok" })))
}

/// GET /cameras/status (all cameras) — return device statuses from observation store.
pub async fn cameras_status_all(
    _auth: CareAuth,
    State(state): State<AppState>,
) -> Json<Value> {
    let statuses = state.obs_store.get_device_statuses();
    Json(json!({
        "time": Utc::now().to_rfc3339(),
        "status": statuses
    }))
}
