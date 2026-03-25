use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::state::AppState;

/// GET /healthz — basic health check
pub async fn healthz(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    // Check database connectivity
    let db_status = sqlx::query("SELECT 1")
        .fetch_one(&state.db)
        .await
        .map(|_| "ok")
        .unwrap_or("error");

    Ok(Json(json!({
        "server": "ok",
        "database": db_status
    })))
}

/// GET /health/ping
pub async fn ping() -> Json<Value> {
    Json(json!({
        "pong": Utc::now().to_rfc3339()
    }))
}

/// GET /health/status
pub async fn status(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db_status = sqlx::query("SELECT 1")
        .fetch_one(&state.db)
        .await
        .map(|_| "ok")
        .unwrap_or("error");

    Ok(Json(json!({
        "server": "ok",
        "database": db_status,
        "version": state.settings.app_version
    })))
}

/// GET /health/care/communication — proxy to CARE /middleware/verify
pub async fn care_communication(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let url = format!(
        "{}/middleware/verify",
        state.settings.care_api.trim_end_matches('/')
    );
    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CareApi(format!("unreachable: {e}")))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| json!({ "status": status.as_u16() }));

    Ok(Json(body))
}

/// GET /health/care/communication-asset — proxy with asset JWT
pub async fn care_communication_asset(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let token = state
        .own_keypair
        .sign_jwt(serde_json::json!({}), 300)
        .map_err(|e| AppError::Internal(e))?;

    let url = format!(
        "{}/middleware/verify",
        state.settings.care_api.trim_end_matches('/')
    );
    let resp = state
        .http
        .get(&url)
        .header("Authorization", format!("Gateway_Bearer {token}"))
        .header("X-Gateway-Id", &state.settings.gateway_device_id)
        .send()
        .await
        .map_err(|e| AppError::CareApi(format!("unreachable: {e}")))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| json!({ "status": status.as_u16() }));

    Ok(Json(body))
}
