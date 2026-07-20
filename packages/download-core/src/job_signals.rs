//! Per-job pause / transfer signals reachable without the Core mutex.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct JobSignals {
    pause_requested: Mutex<HashMap<String, Arc<AtomicBool>>>,
    transfer_active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl JobSignals {
    pub fn register(
        &self,
        job_id: &str,
        pause_requested: Arc<AtomicBool>,
        transfer_active: Arc<AtomicBool>,
    ) {
        self.pause_requested
            .lock()
            .unwrap()
            .insert(job_id.to_string(), pause_requested);
        self.transfer_active
            .lock()
            .unwrap()
            .insert(job_id.to_string(), transfer_active);
    }

    pub fn unregister(&self, job_id: &str) {
        self.pause_requested.lock().unwrap().remove(job_id);
        self.transfer_active.lock().unwrap().remove(job_id);
    }

    pub fn request_pause(&self, job_id: &str) -> Option<Arc<AtomicBool>> {
        self.pause_requested
            .lock()
            .unwrap()
            .get(job_id)
            .cloned()
    }

    pub fn is_transfer_active(&self, job_id: &str) -> bool {
        self.transfer_active
            .lock()
            .unwrap()
            .get(job_id)
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}
