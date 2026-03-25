use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::broadcast;

use super::types::{DeviceStatus, Observation, ObservationId, StaticObservation};
use super::validity;

/// Maximum observations to keep per device (approx 2 hours at 1/sec).
const MAX_BUFFER_SIZE: usize = 7200;
/// Broadcast channel capacity per device.
const CHANNEL_CAPACITY: usize = 256;

/// In-memory observation store with per-device ring buffers and broadcast channels.
pub struct ObservationStore {
    /// Ring buffer per device_id.
    buffers: DashMap<String, VecDeque<Observation>>,
    /// Broadcast sender per device_id for WebSocket push.
    channels: DashMap<String, broadcast::Sender<Vec<Observation>>>,
    /// Last known device status per device_id.
    device_status: DashMap<String, DeviceStatus>,
    /// Last blood pressure reading per device_id for carry-forward.
    last_bp: DashMap<String, Observation>,
}

impl ObservationStore {
    pub fn new() -> Self {
        Self {
            buffers: DashMap::new(),
            channels: DashMap::new(),
            device_status: DashMap::new(),
            last_bp: DashMap::new(),
        }
    }

    /// Ingest a batch of observations (called by POST /update_observations).
    pub fn ingest(&self, mut observations: Vec<Observation>) {
        if observations.is_empty() {
            return;
        }

        let now = Utc::now();

        // Set taken_at timestamp
        for obs in &mut observations {
            obs.taken_at = now;
        }

        // Group by device_id
        let mut by_device: HashMap<String, Vec<Observation>> = HashMap::new();
        for obs in observations {
            by_device
                .entry(obs.device_id.clone())
                .or_default()
                .push(obs);
        }

        for (device_id, mut device_obs) in by_device {
            // Track last BP for carry-forward
            for obs in &device_obs {
                if obs.observation_id == ObservationId::BloodPressure && validity::is_valid(obs) {
                    self.last_bp.insert(device_id.clone(), obs.clone());
                }
            }

            // Blood pressure carry-forward: if the batch lacks a BP reading,
            // append the last known one.
            let has_bp = device_obs
                .iter()
                .any(|o| o.observation_id == ObservationId::BloodPressure);
            if !has_bp {
                if let Some(bp) = self.last_bp.get(&device_id) {
                    device_obs.push(bp.clone());
                }
            }

            // Update device status
            self.device_status.insert(
                device_id.clone(),
                DeviceStatus {
                    status: "up".into(),
                    time: now,
                },
            );

            // Append to ring buffer
            let mut buf = self
                .buffers
                .entry(device_id.clone())
                .or_insert_with(VecDeque::new);
            for obs in &device_obs {
                buf.push_back(obs.clone());
                while buf.len() > MAX_BUFFER_SIZE {
                    buf.pop_front();
                }
            }

            // Broadcast to WebSocket subscribers
            let tx = self
                .channels
                .entry(device_id)
                .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
            // Ignore send errors (no subscribers).
            let _ = tx.send(device_obs);
        }
    }

    /// Get the latest static (non-waveform) observations for a device within a given duration.
    /// Used by the automated observations task.
    pub fn get_static(&self, device_id: &str, since: Duration) -> Option<StaticObservation> {
        let buf = self.buffers.get(device_id)?;
        let cutoff = Utc::now() - chrono::Duration::from_std(since).ok()?;

        let mut latest: HashMap<ObservationId, Observation> = HashMap::new();
        for obs in buf.iter().rev() {
            if obs.taken_at < cutoff {
                break;
            }
            // Skip waveform types
            match obs.observation_id {
                ObservationId::Waveform
                | ObservationId::WaveformII
                | ObservationId::WaveformPleth
                | ObservationId::WaveformRespiration => continue,
                _ => {}
            }
            // Keep only the latest per observation type
            latest
                .entry(obs.observation_id.clone())
                .or_insert_with(|| obs.clone());
        }

        if latest.is_empty() {
            return None;
        }

        let last_updated = latest.values().map(|o| o.taken_at).max().unwrap_or(cutoff);

        Some(StaticObservation {
            device_id: device_id.to_string(),
            observations: latest.into_values().collect(),
            last_updated,
        })
    }

    /// Subscribe to a device's observation broadcast channel (for WebSocket push).
    pub fn subscribe(&self, device_id: &str) -> broadcast::Receiver<Vec<Observation>> {
        let tx = self
            .channels
            .entry(device_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        tx.subscribe()
    }

    /// Get current device statuses.
    pub fn get_device_statuses(&self) -> HashMap<String, DeviceStatus> {
        self.device_status
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Update a specific device's status (used by camera status sweep).
    pub fn set_device_status(&self, device_id: String, status: String) {
        self.device_status.insert(
            device_id,
            DeviceStatus {
                status,
                time: Utc::now(),
            },
        );
    }

    /// Drain observations older than the given duration (for S3 dump).
    pub fn drain_stale(&self, older_than: Duration) -> Vec<Observation> {
        let cutoff = Utc::now() - chrono::Duration::from_std(older_than).unwrap_or_default();
        let mut stale = Vec::new();

        for mut entry in self.buffers.iter_mut() {
            while let Some(front) = entry.front() {
                if front.taken_at < cutoff {
                    if let Some(obs) = entry.pop_front() {
                        stale.push(obs);
                    }
                } else {
                    break;
                }
            }
        }

        stale
    }
}

impl Default for ObservationStore {
    fn default() -> Self {
        Self::new()
    }
}
