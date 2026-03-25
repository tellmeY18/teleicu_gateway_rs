use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use crate::auth::outbound::OwnKeypair;
use crate::error::AppError;

/// Typed HTTP client for the CARE API.
pub struct CareClient {
    http: reqwest::Client,
    base_url: String,
    timeout: Duration,
    keypair: Arc<OwnKeypair>,
    gateway_device_id: String,
}

impl CareClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        timeout_secs: u64,
        keypair: Arc<OwnKeypair>,
        gateway_device_id: String,
    ) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(timeout_secs),
            keypair,
            gateway_device_id,
        }
    }

    /// Build auth headers: Gateway_Bearer JWT + X-Gateway-Id.
    fn headers(&self, extra_claims: Option<Value>) -> Result<HeaderMap, AppError> {
        let claims = extra_claims.unwrap_or_else(|| json!({}));
        let token = self
            .keypair
            .sign_jwt(claims, 300) // 5-minute expiry for API calls
            .map_err(AppError::Internal)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Gateway_Bearer {token}"))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("header error: {e}")))?,
        );
        headers.insert(
            "X-Gateway-Id",
            HeaderValue::from_str(&self.gateway_device_id)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("header error: {e}")))?,
        );
        Ok(headers)
    }

    /// GET request to CARE API.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        let url = format!("{}{}", self.base_url, path);
        let headers = self.headers(None)?;

        let resp = self
            .http
            .get(&url)
            .headers(headers)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AppError::CareApi("timeout".into())
                } else if e.is_connect() {
                    AppError::CareApi("unreachable".into())
                } else {
                    AppError::CareApi(format!("request failed: {e}"))
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::CareApi(format!("HTTP {status}: {body}")));
        }

        resp.json::<T>()
            .await
            .map_err(|e| AppError::CareApi(format!("failed to decode response: {e}")))
    }

    /// POST request to CARE API.
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AppError> {
        let url = format!("{}{}", self.base_url, path);
        let headers = self.headers(None)?;

        let resp = self
            .http
            .post(&url)
            .headers(headers)
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AppError::CareApi("timeout".into())
                } else if e.is_connect() {
                    AppError::CareApi("unreachable".into())
                } else {
                    AppError::CareApi(format!("request failed: {e}"))
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::CareApi(format!("HTTP {status}: {body}")));
        }

        resp.json::<T>()
            .await
            .map_err(|e| AppError::CareApi(format!("failed to decode response: {e}")))
    }
}
