use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::time;

use crate::care_client::CareClient;
use crate::observations::types::{
    observation_code, unit_code, Coding, Observation, ObservationId, ObservationValue,
    ReferenceRange, AUTOMATED_OBSERVATION_TYPES,
};
use crate::observations::validity;
use crate::state::AppState;

/// FHIR-ish observation record for CARE API.
#[derive(Debug, Clone, Serialize)]
pub struct ObservationWriteSpec {
    pub status: String,
    pub category: Coding,
    pub main_code: Coding,
    pub effective_datetime: DateTime<Utc>,
    pub value_type: String,
    pub value: ObservationValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub reference_range: Vec<ReferenceRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpretation: Option<String>,
}

/// Monitor entry from CARE's automated observations endpoint.
#[derive(Debug, Deserialize)]
struct MonitorEntry {
    id: String,
    #[serde(rename = "endpoint_address")]
    endpoint_address: Option<String>,
}

/// Background loop: periodically push automated observations to CARE.
pub async fn run_loop(state: AppState) {
    if !state.settings.automated_observations_enabled {
        tracing::info!("Automated observations disabled, skipping task");
        return;
    }
    if state.settings.gateway_device_id.is_empty() {
        tracing::info!("No GATEWAY_DEVICE_ID configured, skipping automated observations");
        return;
    }

    let interval_mins = state.settings.automated_observations_interval_mins;
    let interval = Duration::from_secs(interval_mins * 60);
    let mut ticker = time::interval(interval);

    // Skip first immediate tick
    ticker.tick().await;

    let care = CareClient::new(
        state.http.clone(),
        state.settings.care_api.clone(),
        state.settings.care_api_timeout_secs,
        state.own_keypair.clone(),
        state.settings.gateway_device_id.clone(),
    );

    loop {
        ticker.tick().await;
        tracing::info!("Running automated observations cycle");

        if let Err(e) = run_once(&state, &care, interval).await {
            tracing::error!("Automated observations error: {e}");
        }
    }
}

async fn run_once(
    state: &AppState,
    care: &CareClient,
    interval: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    // Fetch list of monitors from CARE
    let monitors: Vec<MonitorEntry> = care
        .get("/api/vitals_observation_device/automated_observations/")
        .await?;

    for monitor in &monitors {
        let endpoint = match &monitor.endpoint_address {
            Some(ep) if !ep.is_empty() => ep.clone(),
            _ => continue,
        };

        // Get recent observations from in-memory store
        let static_obs = match state.obs_store.get_static(&endpoint, interval) {
            Some(obs) => obs,
            None => {
                tracing::debug!("No recent observations for {endpoint}, skipping");
                continue;
            }
        };

        // Build FHIR-ish records
        let specs = build_observation_specs(&static_obs.observations);
        if specs.is_empty() {
            tracing::debug!("No valid observations to ship for {endpoint}");
            continue;
        }

        // POST to CARE
        let path = format!(
            "/api/vitals_observation_device/automated_observations/{}/record/",
            monitor.id
        );
        match care.post::<_, Value>(&path, &specs).await {
            Ok(_) => tracing::info!("Shipped {} observations for {endpoint}", specs.len()),
            Err(e) => tracing::error!("Failed to ship observations for {endpoint}: {e}"),
        }
    }

    Ok(())
}

/// Build FHIR-ish observation specs from raw observations.
fn build_observation_specs(observations: &[Observation]) -> Vec<ObservationWriteSpec> {
    let category = Coding {
        system: "http://terminology.hl7.org/CodeSystem/observation-category".into(),
        code: "vital-signs".into(),
        display: "Vital Signs".into(),
    };

    let mut specs = Vec::new();

    for obs in observations {
        // Only ship allowed types
        if !AUTOMATED_OBSERVATION_TYPES.contains(&obs.observation_id) {
            continue;
        }

        // Only valid observations
        if !validity::is_valid(obs) {
            continue;
        }

        let main_code = match observation_code(&obs.observation_id) {
            Some(c) => c,
            None => continue,
        };
        let unit = match unit_code(&obs.observation_id) {
            Some(u) => u,
            None => continue,
        };

        // Blood pressure: ship systolic value
        if obs.observation_id == ObservationId::BloodPressure {
            if let Some(ref sys) = obs.systolic {
                if let Some(val) = sys.value {
                    let mut ref_ranges = Vec::new();
                    if sys.low_limit.is_some() || sys.high_limit.is_some() {
                        ref_ranges.push(ReferenceRange {
                            low: sys.low_limit.map(|v| v.to_string()),
                            high: sys.high_limit.map(|v| v.to_string()),
                        });
                    }

                    specs.push(ObservationWriteSpec {
                        status: "final".into(),
                        category: category.clone(),
                        main_code,
                        effective_datetime: obs.date_time,
                        value_type: "decimal".into(),
                        value: ObservationValue {
                            value: val.to_string(),
                            unit,
                        },
                        note: None,
                        reference_range: ref_ranges,
                        interpretation: obs
                            .interpretation
                            .as_ref()
                            .map(|i| format!("{:?}", i).to_lowercase()),
                    });
                }
            }
            continue;
        }

        // Numeric observations
        if let Some(val) = obs.value {
            let value_type = if val.fract() == 0.0 {
                "integer"
            } else {
                "decimal"
            };

            let mut ref_ranges = Vec::new();
            if obs.low_limit.is_some() || obs.high_limit.is_some() {
                ref_ranges.push(ReferenceRange {
                    low: obs.low_limit.map(|v| v.to_string()),
                    high: obs.high_limit.map(|v| v.to_string()),
                });
            }

            specs.push(ObservationWriteSpec {
                status: "final".into(),
                category: category.clone(),
                main_code,
                effective_datetime: obs.date_time,
                value_type: value_type.into(),
                value: ObservationValue {
                    value: val.to_string(),
                    unit,
                },
                note: None,
                reference_range: ref_ranges,
                interpretation: obs
                    .interpretation
                    .as_ref()
                    .map(|i| format!("{:?}", i).to_lowercase()),
            });
        }
    }

    specs
}
