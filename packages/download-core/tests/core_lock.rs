//! L-49: a panic under the Core lock must not brick the shell for good.

use download_core::*;
use std::sync::{Arc, Mutex};

struct NoHttp;

impl HttpBackend for NoHttp {
    fn fetch(
        &self,
        _url: &str,
        _range: Option<std::ops::Range<u64>>,
        _sink: &mut dyn std::io::Write,
    ) -> Result<FetchResult, String> {
        Err("offline".into())
    }
    fn supports_range(&self, _url: &str) -> Result<bool, String> {
        Err("offline".into())
    }
    fn file_size(&self, _url: &str) -> Result<u64, String> {
        Err("offline".into())
    }
}

fn poisoned_core() -> Arc<Mutex<Core>> {
    let (core, _rx) = Core::new(Box::new(NoHttp), Box::new(FakeExtractorToolchain::new()));
    let core = Arc::new(Mutex::new(core));
    let held = Arc::clone(&core);
    let crashed = std::thread::spawn(move || {
        let _guard = held.lock().unwrap();
        panic!("a command panicked under the Core lock");
    })
    .join();
    assert!(crashed.is_err());
    assert!(core.lock().is_err(), "the mutex should be poisoned");
    core
}

#[test]
fn lock_core_recovers_a_poisoned_mutex() {
    let core = poisoned_core();
    let guard = lock_core(&core);
    assert_eq!(guard.history_len(), 0);
}

#[test]
fn outside_lock_helpers_keep_working_after_a_poisoning_panic() {
    let core = poisoned_core();
    // These used to return "poisoned lock: another task failed inside".
    Core::toolchain_status_outside_lock(&core).expect("status after poisoning");
    Core::apply_discovered_config_outside_lock(&core, YtDlpConfig::default());
}
