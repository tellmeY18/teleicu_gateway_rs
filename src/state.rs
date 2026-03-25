use std::sync::Arc;
use tokio::sync::RwLock;

use crate::auth::outbound::OwnKeypair;
use crate::config::Settings;
use crate::observations::store::ObservationStore;
use crate::onvif::lock::CameraLockMap;

/// Cached JWKS from CARE for inbound JWT validation.
#[derive(Clone)]
pub struct CachedJwks {
    pub keys: jsonwebtoken::jwk::JwkSet,
    pub fetched_at: tokio::time::Instant,
}

/// Shared application state, cloned into every handler via Axum.
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub settings: Arc<Settings>,
    pub http: reqwest::Client,
    pub obs_store: Arc<ObservationStore>,
    pub camera_locks: Arc<CameraLockMap>,
    pub care_jwks_cache: Arc<RwLock<Option<CachedJwks>>>,
    pub own_keypair: Arc<OwnKeypair>,
}
