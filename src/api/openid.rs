use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::state::AppState;

/// GET /openid-configuration/ — return the gateway's public JWKS.
/// No auth required. This is how CARE verifies the gateway's outbound tokens.
pub async fn openid_configuration(State(state): State<AppState>) -> Json<Value> {
    Json(state.own_keypair.public_jwks())
}
