use aws_config::BehaviorVersion;
use aws_sdk_s3::Client as S3Client;
use chrono::Utc;
use std::time::Duration;
use tokio::time;

use crate::state::AppState;

/// Background loop: dump stale observations to S3 every 30 minutes.
pub async fn run_loop(state: AppState) {
    if !state.settings.s3_configured() {
        tracing::info!("S3 not configured, skipping S3 dump task");
        return;
    }

    let interval = Duration::from_secs(30 * 60); // 30 minutes
    let mut ticker = time::interval(interval);

    // Skip first immediate tick
    ticker.tick().await;

    // Build S3 client
    let s3_client = match build_s3_client(&state).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to build S3 client: {e}");
            return;
        }
    };

    let bucket = state.settings.s3_bucket_name.clone().unwrap_or_default();
    let host_name = state.settings.host_name.clone();

    loop {
        ticker.tick().await;
        tracing::debug!("Running S3 dump");

        let stale = state.obs_store.drain_stale(interval);
        if stale.is_empty() {
            tracing::debug!("No stale observations to dump");
            continue;
        }

        let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
        let key = format!("{host_name}/{timestamp}.json");

        let body = match serde_json::to_vec(&stale) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to serialize observations: {e}");
                continue;
            }
        };

        match s3_client
            .put_object()
            .bucket(&bucket)
            .key(&key)
            .body(body.into())
            .content_type("application/json")
            .send()
            .await
        {
            Ok(_) => tracing::info!("Dumped {} observations to s3://{bucket}/{key}", stale.len()),
            Err(e) => tracing::error!("S3 put_object failed: {e}"),
        }
    }
}

/// Build an S3 client from the app settings.
async fn build_s3_client(state: &AppState) -> Result<S3Client, Box<dyn std::error::Error>> {
    let access_key = state
        .settings
        .s3_access_key_id
        .as_deref()
        .unwrap_or_default();
    let secret_key = state
        .settings
        .s3_secret_access_key
        .as_deref()
        .unwrap_or_default();

    let creds = aws_sdk_s3::config::Credentials::new(
        access_key,
        secret_key,
        None,
        None,
        "teleicu-gateway",
    );

    let mut config_builder = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .credentials_provider(creds)
        .region(aws_sdk_s3::config::Region::new("us-east-1"));

    // Custom endpoint for S3-compatible stores (MinIO etc.)
    if let Some(endpoint) = &state.settings.s3_endpoint_url {
        config_builder = config_builder
            .endpoint_url(endpoint)
            .force_path_style(true);
    }

    let config = config_builder.build();
    Ok(S3Client::from_conf(config))
}
