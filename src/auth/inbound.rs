use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Instant;

use crate::error::AppError;
use crate::state::{AppState, CachedJwks};

/// TTL for the cached JWKS from CARE (5 minutes).
const JWKS_CACHE_TTL_SECS: u64 = 300;

/// Claims decoded from a validated CARE JWT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedClaims {
    pub sub: Option<String>,
    pub exp: Option<u64>,
    pub iat: Option<u64>,
    // Allow additional fields
}

/// Axum extractor: validates `Care_Bearer <token>` on incoming requests.
pub struct CareAuth(pub ValidatedClaims);

#[async_trait]
impl FromRequestParts<AppState> for CareAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = header
            .strip_prefix("Care_Bearer ")
            .ok_or(AppError::Unauthorized)?;

        let jwks = fetch_or_cached_jwks(
            &state.http,
            &state.settings.care_api,
            &state.care_jwks_cache,
        )
        .await?;

        // Try each key in the keyset
        for jwk in &jwks.keys {
            let decoding_key = match DecodingKey::from_jwk(jwk) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let mut validation = Validation::new(Algorithm::RS256);
            validation.validate_exp = true;
            // Don't require specific audience or issuer
            validation.set_required_spec_claims(&["exp"]);

            match decode::<ValidatedClaims>(token, &decoding_key, &validation) {
                Ok(data) => return Ok(CareAuth(data.claims)),
                Err(_) => continue,
            }
        }

        Err(AppError::Unauthorized)
    }
}

/// Fetch CARE's JWKS, using the in-memory cache if still valid.
async fn fetch_or_cached_jwks(
    http: &reqwest::Client,
    care_api: &str,
    cache: &Arc<RwLock<Option<CachedJwks>>>,
) -> Result<jsonwebtoken::jwk::JwkSet, AppError> {
    // Check cache
    {
        let guard = cache.read().await;
        if let Some(cached) = guard.as_ref() {
            if cached.fetched_at.elapsed().as_secs() < JWKS_CACHE_TTL_SECS {
                return Ok(cached.keys.clone());
            }
        }
    }

    // Fetch fresh
    let url = format!(
        "{}/api/gateway_device/jwks.json/",
        care_api.trim_end_matches('/')
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CareApi(format!("failed to fetch JWKS: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::CareApi(format!(
            "JWKS fetch returned {}",
            resp.status()
        )));
    }

    let jwks: jsonwebtoken::jwk::JwkSet = resp
        .json()
        .await
        .map_err(|e| AppError::CareApi(format!("failed to parse JWKS: {e}")))?;

    // Update cache
    {
        let mut guard = cache.write().await;
        *guard = Some(CachedJwks {
            keys: jwks.clone(),
            fetched_at: Instant::now(),
        });
    }

    Ok(jwks)
}
