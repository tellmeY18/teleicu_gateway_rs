use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObservationId {
    #[serde(rename = "heart-rate")]
    HeartRate,
    #[serde(rename = "ST")]
    ST,
    #[serde(rename = "SpO2")]
    SpO2,
    #[serde(rename = "pulse-rate")]
    PulseRate,
    #[serde(rename = "respiratory-rate")]
    RespiratoryRate,
    #[serde(rename = "body-temperature1")]
    BodyTemperature1,
    #[serde(rename = "body-temperature2")]
    BodyTemperature2,
    #[serde(rename = "blood-pressure")]
    BloodPressure,
    #[serde(rename = "waveform")]
    Waveform,
    #[serde(rename = "device-connection")]
    DeviceConnection,
    #[serde(rename = "waveform-ii")]
    WaveformII,
    #[serde(rename = "waveform-pleth")]
    WaveformPleth,
    #[serde(rename = "waveform-respiration")]
    WaveformRespiration,
}

impl std::fmt::Display for ObservationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

/// Waveform names used in waveform-type observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WaveName {
    II,
    Pleth,
    Respiration,
}

/// Interpretation values for observation readings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interpretation {
    Normal,
    Low,
    High,
    Critical,
    #[serde(other)]
    Unknown,
}

/// Nested blood pressure sub-reading (systolic, diastolic, or MAP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloodPressureReading {
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub interpretation: Option<Interpretation>,
    #[serde(rename = "low-limit")]
    pub low_limit: Option<f64>,
    #[serde(rename = "high-limit")]
    pub high_limit: Option<f64>,
}

/// Status of a monitored device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub status: String, // "up" or "down"
    pub time: DateTime<Utc>,
}

/// A single observation from a bedside monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    #[serde(rename = "observation_id")]
    pub observation_id: ObservationId,

    #[serde(rename = "device_id")]
    pub device_id: String,

    #[serde(rename = "date-time")]
    pub date_time: DateTime<Utc>,

    #[serde(rename = "patient-id")]
    pub patient_id: String,

    #[serde(rename = "patient-name")]
    pub patient_name: Option<String>,

    pub status: String,

    pub value: Option<f64>,
    pub unit: Option<String>,
    pub interpretation: Option<Interpretation>,

    #[serde(rename = "low-limit")]
    pub low_limit: Option<f64>,
    #[serde(rename = "high-limit")]
    pub high_limit: Option<f64>,

    // Blood pressure nested
    pub systolic: Option<BloodPressureReading>,
    pub diastolic: Option<BloodPressureReading>,
    pub map: Option<BloodPressureReading>,

    // Waveform fields
    #[serde(rename = "wave-name")]
    pub wave_name: Option<WaveName>,
    pub resolution: Option<String>,
    #[serde(rename = "sampling-rate")]
    pub sampling_rate: Option<String>,
    #[serde(rename = "data-baseline")]
    pub data_baseline: Option<f64>,
    #[serde(rename = "data-low-limit")]
    pub data_low_limit: Option<f64>,
    #[serde(rename = "data-high-limit")]
    pub data_high_limit: Option<f64>,
    pub data: Option<String>,

    /// Timestamp set on ingest (server-side), not from the wire.
    #[serde(default = "Utc::now")]
    pub taken_at: DateTime<Utc>,
}

/// Static (non-waveform) observation snapshot for automated observations.
#[derive(Debug, Clone)]
pub struct StaticObservation {
    pub device_id: String,
    pub observations: Vec<Observation>,
    pub last_updated: DateTime<Utc>,
}

/// FHIR coding element used in automated observation payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coding {
    pub system: String,
    pub code: String,
    pub display: String,
}

/// FHIR-style observation value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationValue {
    pub value: String,
    pub unit: Coding,
}

/// FHIR-style reference range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceRange {
    pub low: Option<String>,
    pub high: Option<String>,
}

// ---------- FHIR code mappings ----------

/// Maps ObservationId to a FHIR LOINC-style code.
pub fn observation_code(obs_id: &ObservationId) -> Option<Coding> {
    let (code, display) = match obs_id {
        ObservationId::HeartRate => ("8867-4", "Heart rate"),
        ObservationId::PulseRate => ("8889-8", "Heart rate --by Pulse oximetry"),
        ObservationId::SpO2 => ("2708-6", "Oxygen saturation"),
        ObservationId::RespiratoryRate => ("9279-1", "Respiratory rate"),
        ObservationId::BodyTemperature1 => ("8310-5", "Body temperature"),
        ObservationId::BodyTemperature2 => ("8310-5", "Body temperature"),
        ObservationId::BloodPressure => ("85354-9", "Blood pressure panel"),
        _ => return None,
    };
    Some(Coding {
        system: "http://loinc.org".into(),
        code: code.into(),
        display: display.into(),
    })
}

/// Maps ObservationId to its UCUM unit code.
pub fn unit_code(obs_id: &ObservationId) -> Option<Coding> {
    let (code, display) = match obs_id {
        ObservationId::HeartRate | ObservationId::PulseRate => ("/min", "beats/minute"),
        ObservationId::SpO2 => ("%", "%"),
        ObservationId::RespiratoryRate => ("/min", "breaths/minute"),
        ObservationId::BodyTemperature1 | ObservationId::BodyTemperature2 => ("Cel", "°C"),
        ObservationId::BloodPressure => ("mm[Hg]", "mmHg"),
        _ => return None,
    };
    Some(Coding {
        system: "http://unitsofmeasure.org".into(),
        code: code.into(),
        display: display.into(),
    })
}

/// The observation types that are eligible for automated CARE uploads.
pub const AUTOMATED_OBSERVATION_TYPES: &[ObservationId] = &[
    ObservationId::HeartRate,
    ObservationId::PulseRate,
    ObservationId::SpO2,
    ObservationId::RespiratoryRate,
    ObservationId::BodyTemperature1,
    ObservationId::BodyTemperature2,
    ObservationId::BloodPressure,
];
