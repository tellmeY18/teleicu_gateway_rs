use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::inbound::CareAuth;
use crate::error::AppError;
use crate::state::AppState;

/// POST /getToken/videoFeed request body.
#[derive(Debug, Deserialize)]
pub struct VideoStreamRequest {
    pub ip: String,
    pub stream: String,
    #[serde(rename = "duration")]
    pub _duration: Option<String>,
}

/// POST /getToken/vitals request body.
#[derive(Debug, Deserialize)]
pub struct VitalsTokenRequest {
    pub asset_id: String,
    pub ip: String,
    #[serde(rename = "duration")]
    pub _duration: Option<String>,
}

/// POST /verifyToken request body.
#[derive(Debug, Deserialize)]
pub struct VerifyTokenRequest {
    pub token: String,
    pub ip: Option<String>,
    pub stream: Option<String>,
}

/// POST /verify_token request body.
#[derive(Debug, Deserialize)]
pub struct ExchangeTokenRequest {
    pub token: String,
}

/// Parse duration string (minutes) and clamp to 1–60, default 5.
fn parse_duration_mins(d: &Option<String>) -> u64 {
    d.as_ref()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|m| m.clamp(1, 60))
        .unwrap_or(5)
}

/// POST /getToken/videoFeed — issue a short-lived video stream token.
pub async fn get_video_feed_token(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<VideoStreamRequest>,
) -> Result<Json<Value>, AppError> {
    let duration_mins = parse_duration_mins(&req._duration);
    let exp_secs = duration_mins * 60;

    let token = state
        .own_keypair
        .sign_jwt(
            json!({
                "stream": req.stream,
                "ip": req.ip,
            }),
            exp_secs,
        )
        .map_err(|e| AppError::Internal(e))?;

    Ok(Json(json!({ "token": token })))
}

/// POST /getToken/vitals — issue a short-lived vitals stream token.
pub async fn get_vitals_token(
    _auth: CareAuth,
    State(state): State<AppState>,
    Json(req): Json<VitalsTokenRequest>,
) -> Result<Json<Value>, AppError> {
    let duration_mins = parse_duration_mins(&req._duration);
    let exp_secs = duration_mins * 60;

    let token = state
        .own_keypair
        .sign_jwt(
            json!({
                "asset_id": req.asset_id,
                "ip": req.ip,
            }),
            exp_secs,
        )
        .map_err(|e| AppError::Internal(e))?;

    Ok(Json(json!({ "token": token })))
}

/// POST /verifyToken — verify a stream token and check claims match.
pub async fn verify_token(
    State(state): State<AppState>,
    Json(req): Json<VerifyTokenRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let claims = match state.own_keypair.verify_jwt(&req.token) {
        Ok(c) => c,
        Err(_) => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "status": "0" })),
            ));
        }
    };

    // Check that ip or stream matches the token claims
    let extra = &claims.extra;
    let ip_match = req
        .ip
        .as_ref()
        .map(|ip| extra.get("ip").and_then(|v| v.as_str()) == Some(ip.as_str()))
        .unwrap_or(true);
    let stream_match = req
        .stream
        .as_ref()
        .map(|s| extra.get("stream").and_then(|v| v.as_str()) == Some(s.as_str()))
        .unwrap_or(true);

    if ip_match || stream_match {
        Ok((StatusCode::OK, Json(json!({ "status": "1" }))))
    } else {
        Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "status": "0" })),
        ))
    }
}

/// POST /verify_token — exchange a CARE token for a gateway token.
pub async fn exchange_token(
    State(state): State<AppState>,
    Json(req): Json<ExchangeTokenRequest>,
) -> Result<Json<Value>, AppError> {
    // Forward to CARE to verify the token
    let url = format!(
        "{}/api/v1/auth/token/verify/",
        state.settings.care_api.trim_end_matches('/')
    );
    let resp = state
        .http
        .post(&url)
        .json(&json!({ "token": req.token }))
        .send()
        .await
        .map_err(|e| AppError::CareApi(format!("token verify failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Unauthorized);
    }

    // Issue a gateway JWT (20 min expiry)
    let token = state
        .own_keypair
        .sign_jwt(json!({}), 20 * 60)
        .map_err(|e| AppError::Internal(e))?;

    Ok(Json(json!({ "token": token })))
}
