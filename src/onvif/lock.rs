use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::error::AppError;

/// Guard that releases the camera lock when dropped.
pub struct LockGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// Per-IP async mutex map for camera locking.
pub struct CameraLockMap {
    locks: DashMap<String, Arc<Mutex<()>>>,
    timeout: Duration,
}

impl CameraLockMap {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            locks: DashMap::new(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Try to acquire the lock for a camera IP.
    /// Returns `Err(AppError::CameraLocked)` if the lock cannot be acquired within the timeout.
    pub async fn try_lock(&self, ip: &str) -> Result<LockGuard, AppError> {
        let mutex = self
            .locks
            .entry(ip.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        let result = timeout(self.timeout, mutex.lock_owned()).await;

        match result {
            Ok(guard) => Ok(LockGuard { _guard: guard }),
            Err(_) => Err(AppError::CameraLocked),
        }
    }
}
