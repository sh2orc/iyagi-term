//! Parity gate (ticket I10): every case in
//! `docs/implementation/admission-cases.json` (currently 18 fixtures) must
//! reproduce EXACTLY through `term_core::admission`. The file is the shared
//! spec asset the Python reference (`docs/implementation/verify_spec.py::admission`)
//! also runs, so a failure here means the Rust product implementation
//! drifted from the spec — this test is the gate that prevents that.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use term_contracts::defaults::load_spec_defaults;
use term_contracts::metrics::PressureLevel;
use term_contracts::snapshot::QueueReason;
use term_core::admission::{ActiveWorkload, AdmissionConfig, AdmissionInput, AdmissionRequest};

#[derive(Deserialize)]
struct FixtureActive {
    reservation_bytes: u64,
    resident_bytes: Option<u64>,
    cpu_slots: u32,
}

#[derive(Deserialize)]
struct FixtureRequest {
    reservation_bytes: u64,
    cpu_slots: u32,
}

#[derive(Deserialize)]
struct FixtureSample {
    total_bytes: u64,
    available_bytes: Option<u64>,
    logical_cpus: u32,
    sample_age_ms: u64,
    reconciliation_required: bool,
    pressure: PressureLevel,
    running: Vec<FixtureActive>,
    new: FixtureRequest,
}

#[derive(Deserialize)]
struct FixtureCase {
    id: String,
    set: Value,
    expected: QueueReason,
}

#[derive(Deserialize)]
struct FixtureFile {
    base: Value,
    cases: Vec<FixtureCase>,
}

/// Resolved like `term-contracts/src/defaults.rs`: this crate lives at
/// `<repo>/crates/term-core`, so the repo root is two ancestors up.
fn fixture_path() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest
        .ancestors()
        .nth(2)
        .expect("crate lives at <repo>/crates/term-core");
    repo_root.join("docs/implementation/admission-cases.json")
}

/// Python-reference merge semantics (`sample.update(case['set'])`):
/// top-level key replacement, no deep merge.
fn merged(base: &Value, set: &Value) -> Value {
    let mut sample = base.clone();
    if let (Some(dst), Some(src)) = (sample.as_object_mut(), set.as_object()) {
        for (key, value) in src {
            dst.insert(key.clone(), value.clone());
        }
    }
    sample
}

#[test]
fn all_admission_fixture_cases_match() {
    let defaults = load_spec_defaults().expect("docs/implementation/defaults.json must parse");
    let text = std::fs::read_to_string(fixture_path()).expect("admission-cases.json must exist");
    let file: FixtureFile = serde_json::from_str(&text).expect("fixture schema must parse");
    // The current asset carries 18 cases (the ticket text said 17; the file
    // is normative). The exact count guards against silent truncation.
    assert_eq!(file.cases.len(), 18, "fixture set must stay complete");

    for case in &file.cases {
        let sample: FixtureSample =
            serde_json::from_value(merged(&file.base, &case.set)).expect("case must merge");
        let config = AdmissionConfig::from_defaults(&defaults, sample.logical_cpus);
        let input = AdmissionInput {
            total_bytes: sample.total_bytes,
            available_bytes: sample.available_bytes,
            sample_age_ms: sample.sample_age_ms,
            reconciliation_required: sample.reconciliation_required,
            pressure: sample.pressure,
            active: sample
                .running
                .into_iter()
                .map(|w| ActiveWorkload {
                    reservation_bytes: w.reservation_bytes,
                    resident_bytes: w.resident_bytes,
                    cpu_slots: w.cpu_slots,
                })
                .collect(),
            request: AdmissionRequest {
                reservation_bytes: sample.new.reservation_bytes,
                cpu_slots: sample.new.cpu_slots,
            },
        };
        assert_eq!(
            config.decide(&input),
            case.expected,
            "fixture {:?} diverged from the Python reference",
            case.id
        );
    }
}
