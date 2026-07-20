//! Per-job pause / discard / transfer signals reachable without the Core mutex.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct JobSignals {
    pause_requested: Mutex<HashMap<String, Arc<AtomicBool>>>,
    discard_requested: Mutex<HashMap<String, Arc<AtomicBool>>>,
    transfer_active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl JobSignals {
    pub fn register(
        &self,
        job_id: &str,
        pause_requested: Arc<AtomicBool>,
        discard_requested: Arc<AtomicBool>,
        transfer_active: Arc<AtomicBool>,
    ) {
        self.pause_requested
            .lock()
            .unwrap()
            .insert(job_id.to_string(), pause_requested);
        self.discard_requested
            .lock()
            .unwrap()
            .insert(job_id.to_string(), discard_requested);
        self.transfer_active
            .lock()
            .unwrap()
            .insert(job_id.to_string(), transfer_active);
    }

    pub fn unregister(&self, job_id: &str) {
        self.pause_requested.lock().unwrap().remove(job_id);
        self.discard_requested.lock().unwrap().remove(job_id);
        self.transfer_active.lock().unwrap().remove(job_id);
    }

    pub fn request_pause(&self, job_id: &str) -> Option<Arc<AtomicBool>> {
        self.pause_requested
            .lock()
            .unwrap()
            .get(job_id)
            .cloned()
    }

    pub fn request_discard(&self, job_id: &str) -> Option<Arc<AtomicBool>> {
        self.discard_requested
            .lock()
            .unwrap()
            .get(job_id)
            .cloned()
    }

    /// Raise discard + pause so an in-flight body read stops promptly.
    ///
    /// Returns `true` when a transfer is currently active (finalize deferred).
    pub fn signal_discard_stop(&self, job_id: &str) -> bool {
        if let Some(flag) = self.request_discard(job_id) {
            flag.store(true, Ordering::Release);
        }
        if let Some(flag) = self.request_pause(job_id) {
            flag.store(true, Ordering::Release);
        }
        self.is_transfer_active(job_id)
    }

    pub fn is_transfer_active(&self, job_id: &str) -> bool {
        self.transfer_active
            .lock()
            .unwrap()
            .get(job_id)
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}
