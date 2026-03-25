pub mod automated_observations;
pub mod camera_status;
pub mod s3_dump;

use crate::state::AppState;

/// Spawn all background task loops.
pub fn spawn_all(state: AppState) {
    tokio::spawn(automated_observations::run_loop(state.clone()));
    tokio::spawn(camera_status::run_loop(state.clone()));
    tokio::spawn(s3_dump::run_loop(state));
}
