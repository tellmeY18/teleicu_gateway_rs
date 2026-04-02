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
        tracing::debug!(
            target: "teleicu_gateway::auth",
            "🔐 Validating Care_Bearer token for {} {}",
            parts.method,
            parts.uri.path()
        );

        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                tracing::warn!(
                    target: "teleicu_gateway::auth",
                    "❌ Missing Authorization header"
                );
                AppError::Unauthorized
            })?;

        let token = header
            .strip_prefix("Care_Bearer ")
            .ok_or_else(|| {
                tracing::warn!(
                    target: "teleicu_gateway::auth",
                    "❌ Authorization header does not start with 'Care_Bearer '"
                );
                AppError::Unauthorized
            })?;

        tracing::debug!(
            target: "teleicu_gateway::auth",
            "Extracted Care_Bearer token (length: {})",
            token.len()
        );

        tracing::debug!(
            target: "teleicu_gateway::auth",
            "Fetching CARE JWKS from cache or API"
        );

        let jwks = fetch_or_cached_jwks(
            &state.http,
            &state.settings.care_api,
            &state.care_jwks_cache,
        )
        .await?;

        tracing::debug!(
            target: "teleicu_gateway::auth",
            "Retrieved JWKS with {} keys",
            jwks.keys.len()
        );

        // Try each key in the keyset
        for (idx, jwk) in jwks.keys.iter().enumerate() {
            tracing::trace!(
                target: "teleicu_gateway::auth",
                "Trying JWKS key #{} (kid: {:?})",
                idx,
                jwk.common.key_id
            );

            let decoding_key = match DecodingKey::from_jwk(jwk) {
                Ok(k) => k,
                Err(e) => {
                    tracing::trace!(
                        target: "teleicu_gateway::auth",
                        "Failed to create decoding key from JWK #{}: {}",
                        idx,
                        e
                    );
                    continue;
                }
            };
            let mut validation = Validation::new(Algorithm::RS256);
            validation.validate_exp = true;
            // Don't require specific audience or issuer
            validation.set_required_spec_claims(&["exp"]);

            match decode::<ValidatedClaims>(token, &decoding_key, &validation) {
                Ok(data) => {
                    tracing::info!(
                        target: "teleicu_gateway::auth",
                        "✅ Care_Bearer token validated successfully - sub: {:?}",
                        data.claims.sub
                    );
                    return Ok(CareAuth(data.claims));
                }
                Err(e) => {
                    tracing::trace!(
                        target: "teleicu_gateway::auth",
                        "Key #{} failed to validate token: {}",
                        idx,
                        e
                    );
                    continue;
                }
            }
        }

        tracing::warn!(
            target: "teleicu_gateway::auth",
            "❌ Token validation failed - no valid key found in JWKS"
        );
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
            let age_secs = cached.fetched_at.elapsed().as_secs();
            if age_secs < JWKS_CACHE_TTL_SECS {
                tracing::debug!(
                    target: "teleicu_gateway::auth",
                    "✅ Using cached JWKS (age: {}s, ttl: {}s)",
                    age_secs,
                    JWKS_CACHE_TTL_SECS
                );
                return Ok(cached.keys.clone());
            } else {
                tracing::debug!(
                    target: "teleicu_gateway::auth",
                    "JWKS cache expired (age: {}s, ttl: {}s) - fetching fresh",
                    age_secs,
                    JWKS_CACHE_TTL_SECS
                );
            }
        } else {
            tracing::debug!(
                target: "teleicu_gateway::auth",
                "JWKS cache empty - fetching from CARE API"
            );
        }
    }

    // Fetch fresh
    let url = format!(
        "{}/api/gateway_device/jwks.json/",
        care_api.trim_end_matches('/')
    );

    tracing::info!(
        target: "teleicu_gateway::auth",
        "📡 Fetching JWKS from CARE API: {}",
        url
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(
                target: "teleicu_gateway::auth",
                "❌ Failed to fetch JWKS from {}: {}",
                url,
                e
            );
            AppError::CareApi(format!("failed to fetch JWKS: {e}"))
        })?;

    if !resp.status().is_success() {
        tracing::error!(
            target: "teleicu_gateway::auth",
            "❌ CARE API returned error status for JWKS: {}",
            resp.status()
        );
        return Err(AppError::CareApi(format!(
            "JWKS fetch returned {}",
            resp.status()
        )));
    }

    let jwks: jsonwebtoken::jwk::JwkSet = resp
        .json()
        .await
        .map_err(|e| {
            tracing::error!(
                target: "teleicu_gateway::auth",
                "❌ Failed to parse JWKS JSON: {}",
                e
            );
            AppError::CareApi(format!("failed to parse JWKS: {e}"))
        })?;

    tracing::info!(
        target: "teleicu_gateway::auth",
        "✅ JWKS fetched successfully - {} keys",
        jwks.keys.len()
    );

    // Update cache
    {
        let mut guard = cache.write().await;
        *guard = Some(CachedJwks {
            keys: jwks.clone(),
            fetched_at: Instant::now(),
        });
        tracing::debug!(
            target: "teleicu_gateway::auth",
            "JWKS cache updated"
        );
    }

    Ok(jwks)
}
