use std::time::Duration;
use tokio::time;

use crate::db::assets;
use crate::onvif::client::OnvifClient;
use crate::state::AppState;

/// Background loop: sweep camera statuses.
pub async fn run_loop(state: AppState) {
    let interval_secs = state.settings.automated_observations_interval_mins * 60;
    // Default to 5 minutes if the main interval is too large
    let interval_secs = interval_secs.min(300);
    let mut ticker = time::interval(Duration::from_secs(interval_secs));

    // Skip first immediate tick
    ticker.tick().await;

    loop {
        ticker.tick().await;
        tracing::debug!("Running camera status sweep");

        if let Err(e) = sweep_once(&state).await {
            tracing::error!("Camera status sweep error: {e}");
        }
    }
}

async fn sweep_once(state: &AppState) -> Result<(), Box<dyn std::error::Error>> {
    let cameras = assets::list_assets(&state.db, Some("ONVIF")).await?;

    for camera in &cameras {
        let username = camera.username.as_deref().unwrap_or("");
        let password = if let Some(ref enc) = camera.password_enc {
            if let Some(ref key) = state.settings.encryption_key {
                assets::decrypt_password(enc, key).unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let client = OnvifClient::new(
            state.http.clone(),
            &camera.ip_address,
            camera.port as u16,
            username,
            &password,
        );

        let status_str = match client.get_profiles().await {
            Ok(profiles) => {
                if let Some(profile) = profiles.first() {
                    match client.get_status(&profile.token).await {
                        Ok(ptz_status) => {
                            // Treat None or "noerror" as "up"
                            match &ptz_status.error {
                                None => "up".to_string(),
                                Some(_) => "down".to_string(),
                            }
                        }
                        Err(_) => "down".to_string(),
                    }
                } else {
                    "down".to_string()
                }
            }
            Err(_) => "down".to_string(),
        };

        state
            .obs_store
            .set_device_status(camera.ip_address.clone(), status_str);
    }

    Ok(())
}
