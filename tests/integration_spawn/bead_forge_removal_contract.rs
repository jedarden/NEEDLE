//! ADR-021 bead-forge removal contract, driven through the compiled binary.
//!
//! bead-forge (bf) support was removed deliberately
//! (`docs/adr/021-bead-forge-removal.md`): exactly one backend (`bead`,
//! bead-rs) remains, and every surface that can still name bead-forge must
//! fail closed with the migration message — never write a binding, never
//! silently fall back to bead-rs. These tests spawn `needle` so the contract
//! is asserted at the process boundary an operator hits, as a lane separate
//! from the bead-rs lifecycle coverage in `real_br_integration_tests`.
//!
//! Contract surfaces:
//! 1. `needle init --backend bead-forge` refuses and writes nothing.
//! 2. `needle bead-backend-bind bead-forge` refuses and leaves the
//!    workspace's binding untouched (and mints none when absent).
//! 3. `needle bead-backend bead-forge` refuses the capability probe.
//! 4. A workspace whose `.needle.yaml` binds bead-forge fails the full
//!    config resolution — the same load every worker start performs — rather
//!    than degrading the binding to bead-rs or auto.
//! 5. Positive controls: a bead-rs binding still resolves from workspace
//!    configuration, and the default init binding is bead-rs.

use super::isolation::IsolatedChildEnv;

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// `needle init --backend bead-forge` must refuse before writing anything:
/// the binding it used to write is rejected by every later config load, so
/// accepting it here would mint a poisoned workspace.
#[test]
fn init_refuses_bead_forge_binding_without_writing() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("bind-target");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");

    let output = fixture
        .needle()
        .current_dir(&workspace)
        .args(["init", "--backend", "bead-forge", "--no-agents-md"])
        .output()
        .expect("spawn needle init");

    assert!(
        !output.status.success(),
        "init --backend bead-forge must fail: {}",
        combined_output(&output)
    );
    let output_text = combined_output(&output);
    assert!(
        output_text.contains("no longer supported"),
        "refusal must carry the ADR-021 migration message: {output_text}"
    );
    assert!(
        !workspace.join(".needle.yaml").exists(),
        "a refused binding must not be written"
    );
}

#[test]
fn init_refuses_bf_alias_with_the_migration_message() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("bf-bind-target");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");

    let output = fixture
        .needle()
        .current_dir(&workspace)
        .args(["init", "--backend", "bf", "--no-agents-md"])
        .output()
        .expect("spawn needle init");

    assert!(!output.status.success(), "init --backend bf must fail");
    assert!(
        combined_output(&output).contains("no longer supported"),
        "bf refusal must carry the migration message"
    );
    assert!(!workspace.join(".needle.yaml").exists());
}

/// `needle bead-backend-bind bead-forge` must refuse before touching the
/// workspace: an existing bead-rs binding stays byte-identical, and no
/// binding is minted for a workspace without one.
#[test]
fn bind_refuses_bead_forge_and_leaves_binding_untouched() {
    let fixture = IsolatedChildEnv::new();

    let bound = fixture.path().join("bound-workspace");
    std::fs::create_dir_all(bound.join(".beads")).expect("create bound workspace fixture");
    let existing = "bead_cli:\n  backend: bead-rs\n";
    let bound_config = bound.join(".needle.yaml");
    std::fs::write(&bound_config, existing).expect("write existing binding");

    let output = fixture
        .needle()
        .args(["bead-backend-bind", "bead-forge"])
        .arg(&bound)
        .output()
        .expect("spawn needle bead-backend-bind");

    assert!(
        !output.status.success(),
        "bind bead-forge must fail: {}",
        combined_output(&output)
    );
    assert!(
        combined_output(&output).contains("no longer supported"),
        "refusal must carry the ADR-021 migration message"
    );
    assert_eq!(
        std::fs::read_to_string(&bound_config).expect("reread binding"),
        existing,
        "a refused bind must not modify the existing binding"
    );

    let unbound = fixture.path().join("unbound-workspace");
    std::fs::create_dir_all(unbound.join(".beads")).expect("create unbound workspace fixture");
    let output = fixture
        .needle()
        .args(["bead-backend-bind", "bead-forge"])
        .arg(&unbound)
        .output()
        .expect("spawn needle bead-backend-bind");
    assert!(
        !output.status.success(),
        "bind bead-forge must fail on an unbound workspace too"
    );
    assert!(
        !unbound.join(".needle.yaml").exists(),
        "a refused bind must not mint a binding"
    );
}

#[test]
fn bind_refuses_bf_alias_without_writing() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("bf-bind-target");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");

    let output = fixture
        .needle()
        .args(["bead-backend-bind", "bf"])
        .arg(&workspace)
        .output()
        .expect("spawn needle bead-backend-bind");

    assert!(!output.status.success(), "bind bf must fail");
    assert!(combined_output(&output).contains("no longer supported"));
    assert!(!workspace.join(".needle.yaml").exists());
}

/// `needle bead-backend bead-forge` refuses the capability probe with the
/// migration message even though the token is still parseable.
#[test]
fn verify_backend_bead_forge_refused() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("probe-workspace");
    std::fs::create_dir_all(&workspace).expect("create probe workspace fixture");

    let output = fixture
        .needle()
        .args(["bead-backend", "bead-forge"])
        .arg("-w")
        .arg(&workspace)
        .output()
        .expect("spawn needle bead-backend");

    assert!(
        !output.status.success(),
        "verifying bead-forge must fail: {}",
        combined_output(&output)
    );
    assert!(
        combined_output(&output).contains("no longer supported"),
        "refusal must carry the ADR-021 migration message"
    );
}

/// A workspace explicitly bound to bead-forge must fail the full config
/// resolution — the same `load_resolved` every worker start performs —
/// instead of silently degrading the binding to bead-rs or auto.
#[test]
fn config_resolution_rejects_bead_forge_bound_workspace() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("forge-bound-workspace");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-forge\n",
    )
    .expect("write bead-forge binding");

    let output = fixture
        .needle()
        .current_dir(&workspace)
        .args(["config", "--dump"])
        .output()
        .expect("spawn needle config dump");

    assert!(
        !output.status.success(),
        "config resolution must refuse a bead-forge binding: {}",
        combined_output(&output)
    );
    let output_text = combined_output(&output);
    assert!(
        output_text.contains("bead-forge"),
        "refusal must name the offending binding: {output_text}"
    );
    assert!(
        !output_text.contains("backend: bead-rs"),
        "a bead-forge binding must never silently resolve to bead-rs: {output_text}"
    );
}

/// Positive control: the same resolution resolves a bead-rs binding from
/// workspace configuration — the rejection is specific to the removed
/// backend, not to workspace-bound configuration.
#[test]
fn config_resolution_resolves_bead_rs_bound_workspace() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("rs-bound-workspace");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");
    std::fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .expect("write bead-rs binding");

    let output = fixture
        .needle()
        .args(["doctor", "--workspace"])
        .arg(&workspace)
        .arg("--json")
        .output()
        .expect("spawn needle doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Bead CLI Backend") && stdout.contains("bead-rs"),
        "doctor output must show the workspace-bound backend: {stdout}"
    );
}

/// Positive control: the default `needle init` binding is bead-rs and the
/// command still succeeds — the removal contract narrows the accepted
/// backend, it does not break the binding flow.
#[test]
fn init_default_still_binds_bead_rs() {
    let fixture = IsolatedChildEnv::new();
    let workspace = fixture.path().join("fresh-workspace");
    std::fs::create_dir_all(workspace.join(".beads")).expect("create workspace fixture");

    let output = fixture
        .needle()
        .current_dir(&workspace)
        .args(["init", "--no-agents-md"])
        .output()
        .expect("spawn needle init");

    assert!(
        output.status.success(),
        "default init must succeed: {}",
        combined_output(&output)
    );
    let binding = std::fs::read_to_string(workspace.join(".needle.yaml"))
        .expect("default init writes the workspace binding");
    assert!(
        binding.contains("backend: bead-rs"),
        "default binding must be bead-rs: {binding}"
    );
}
