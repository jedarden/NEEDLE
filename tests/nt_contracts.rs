//! Focused N-T behavioral contracts, one module per former dedicated target.
//!
//! Each module below was its own `[[test]]` target until `needle-1f31332d`.
//! Every target statically linked the whole `needle` crate, so 21 of them
//! put 21 extra copies of it into the needle-ci nextest archive. They now
//! share this one binary and keep their files, names and assertions.
//!
//! # Selecting one contract
//!
//! Test names carry the module as a prefix, so the former
//! `cargo test --test nt52_state_dir_isolation` is now
//!
//! ```text
//! cargo test --test nt_contracts nt52_state_dir_isolation::
//! cargo nextest run --test nt_contracts nt52_state_dir_isolation::
//! ```
//!
//! # Process environment
//!
//! nextest gives every test its own process, but plain `cargo test` runs the
//! whole binary in one process. Three modules (`nt10_context_adapter_parity`,
//! `nt51_gateway_health`, `nt52_state_dir_isolation`) swap `HOME`, `TMPDIR`,
//! `NEEDLE_STATE_DIR` and related variables, which every other module reads
//! implicitly through `tempfile`, the state-dir resolver, or the environment
//! a spawned `needle` inherits. So every test here carries exactly one
//! `serial_test` attribute on the `nt_process_env` key:
//!
//! - `#[serial_test::serial(nt_process_env)]` on every test in those three
//!   modules, which excludes every other test while it runs;
//! - `#[serial_test::parallel(nt_process_env)]` on every other test, which
//!   still run concurrently with each other.
//!
//! A new test in this target takes the same attribute: `serial` if it sets or
//! removes a process environment variable, `parallel` otherwise.

#[path = "nt_contracts/admission_support.rs"]
mod admission_support;

#[path = "nt_contracts/nt07_admission_budgets.rs"]
mod nt07_admission_budgets;
#[path = "nt_contracts/nt07_admission_policy.rs"]
mod nt07_admission_policy;
#[path = "nt_contracts/nt07_executable_admission.rs"]
mod nt07_executable_admission;
#[path = "nt_contracts/nt07_impact_contract.rs"]
mod nt07_impact_contract;
#[path = "nt_contracts/nt07_impact_scoring.rs"]
mod nt07_impact_scoring;
#[path = "nt_contracts/nt07_proposal_contract.rs"]
mod nt07_proposal_contract;
#[path = "nt_contracts/nt10_context_adapter_parity.rs"]
mod nt10_context_adapter_parity;
#[path = "nt_contracts/nt10_context_manifest.rs"]
mod nt10_context_manifest;
#[path = "nt_contracts/nt10_policy_admission.rs"]
mod nt10_policy_admission;
#[path = "nt_contracts/nt10_policy_doctor.rs"]
mod nt10_policy_doctor;
#[path = "nt_contracts/nt10_policy_hashing.rs"]
mod nt10_policy_hashing;
#[path = "nt_contracts/nt10_policy_precedence.rs"]
mod nt10_policy_precedence;
#[path = "nt_contracts/nt45_failure_evidence_capture.rs"]
mod nt45_failure_evidence_capture;
#[path = "nt_contracts/nt50_exception_lessons.rs"]
mod nt50_exception_lessons;
#[path = "nt_contracts/nt51_adapter_usage_capture.rs"]
mod nt51_adapter_usage_capture;
#[path = "nt_contracts/nt51_gateway_health.rs"]
mod nt51_gateway_health;
#[path = "nt_contracts/nt52_state_dir_isolation.rs"]
mod nt52_state_dir_isolation;
#[path = "nt_contracts/nt53_improvement_proposals.rs"]
mod nt53_improvement_proposals;
#[path = "nt_contracts/nt54_proposal_admission.rs"]
mod nt54_proposal_admission;
#[path = "nt_contracts/nt55_impact_receipts.rs"]
mod nt55_impact_receipts;
#[path = "nt_contracts/nt56_improvements_cli.rs"]
mod nt56_improvements_cli;
