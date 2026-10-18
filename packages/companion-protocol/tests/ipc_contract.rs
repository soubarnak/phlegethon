//! Pins the serde shape of `CompanionResult` to the sample file shared with the frontend and the
//! shell crate (see `packages/download-core/tests/ipc_contract.rs`). M-71.

use companion_protocol::CompanionResult;
use serde_json::Value;

fn samples() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tauri-shell/frontend/src/tauri/contract/samples.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read samples.json"))
        .expect("parse samples.json")
}

#[test]
fn companion_result_matches_the_samples() {
    let results = vec![
        CompanionResult::EnqueueDirect {
            job_id: "j".into(),
            destination: "/lib/jobs/j".into(),
        },
        CompanionResult::ProbeThenConfirm {
            url: "https://example.com/v".into(),
        },
    ];
    let expected = samples()["CompanionResult"].clone();
    assert_eq!(serde_json::to_value(&results).unwrap(), expected);
    // The extension and the native-messaging host read the same tags back.
    let parsed: Vec<CompanionResult> = serde_json::from_value(expected).unwrap();
    assert_eq!(parsed, results);
}
