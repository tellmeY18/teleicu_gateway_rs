use super::types::{Observation, ObservationId};

/// Sensor-off / error status strings that indicate an invalid reading.
const INVALID_STATUS_STRINGS: &[&str] = &[
    "message-leads off",
    "message-probe off",
    "message-sensor off",
    "message-artifact",
    "message-motion",
    "message-low perfusion",
    "message-searching",
    "message-initializing",
    "message-measurement error",
    "message-no pulse",
    "disconnected",
    "sensor off",
    "leads off",
    "probe off",
    "error",
];

/// Returns true if this observation has a valid reading suitable for
/// automated upload to CARE.
pub fn is_valid(obs: &Observation) -> bool {
    // Device-connection observations are always "valid" (they represent status, not a reading).
    if obs.observation_id == ObservationId::DeviceConnection {
        return true;
    }

    // Check status string
    let status_lower = obs.status.to_lowercase();
    for bad in INVALID_STATUS_STRINGS {
        if status_lower.contains(bad) {
            return false;
        }
    }

    // Blood pressure special case: at least systolic value must be present.
    if obs.observation_id == ObservationId::BloodPressure {
        return obs
            .systolic
            .as_ref()
            .and_then(|s| s.value)
            .map(|v| v > 0.0)
            .unwrap_or(false);
    }

    // For all other numeric observations, value must be present and non-zero.
    // Waveform observations are not shipped as automated observations, so this
    // branch is effectively for vitals only.
    match obs.observation_id {
        ObservationId::Waveform
        | ObservationId::WaveformII
        | ObservationId::WaveformPleth
        | ObservationId::WaveformRespiration => true,
        _ => obs.value.map(|v| v > 0.0).unwrap_or(false),
    }
}
