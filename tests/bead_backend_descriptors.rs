use needle::bead_store::{builtin_bead_backends, load_bead_backends, BeadBackend};
use std::fs;
use std::path::Path;

fn write_descriptor(path: &Path, backend: &BeadBackend) {
    fs::write(path, serde_yaml::to_string(backend).unwrap()).unwrap();
}

#[test]
fn shipped_descriptors_are_valid_and_ordered_primary_first() {
    let builtins = builtin_bead_backends();
    assert_eq!(
        builtins
            .iter()
            .map(|backend| backend.name.as_str())
            .collect::<Vec<_>>(),
        ["bead-rs", "bead-forge"]
    );

    for backend in &builtins {
        backend
            .validate(Path::new("<test-builtin>"))
            .unwrap_or_else(|error| panic!("{} descriptor is invalid: {error}", backend.name));
    }
}

#[test]
fn shipped_descriptors_encode_the_observed_dialect_and_capability_differences() {
    let builtins = builtin_bead_backends();
    let bead = builtins
        .iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap();
    let forge = builtins
        .iter()
        .find(|backend| backend.name == "bead-forge")
        .unwrap();

    assert_eq!(bead.identity_pattern, r"^bead\s");
    assert_eq!(forge.identity_pattern, r"^bf\s");
    assert_eq!(
        bead.operations["dep_add"].argv,
        ["dep", "add", "{blocked}", "{blocker}", "--kind", "blocks"]
    );
    assert_eq!(
        forge.operations["dep_add"].argv,
        ["dep", "add", "{blocker}", "--blocks", "{blocked}"]
    );
    assert_eq!(
        bead.operations["split"].strategy.as_deref(),
        Some("sequential")
    );
    assert_eq!(
        forge.operations["split"].strategy.as_deref(),
        Some("transactional_batch")
    );
    assert!(bead.capabilities.atomic_claim);
    assert!(!bead.capabilities.transactional_batch);
    assert!(!bead.capabilities.velocity_metadata);
    assert!(forge.capabilities.atomic_claim);
    assert!(forge.capabilities.transactional_batch);
    assert!(forge.capabilities.velocity_metadata);
}

#[test]
fn user_yaml_overrides_builtin_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let builtins = builtin_bead_backends();
    let mut override_backend = builtins
        .iter()
        .find(|backend| backend.name == "bead-rs")
        .unwrap()
        .clone();
    override_backend.binary = "custom-bead".to_string();
    override_backend.verified_against = "custom build".to_string();
    write_descriptor(&directory.path().join("override.yaml"), &override_backend);

    let loaded = load_bead_backends(directory.path(), &builtins).unwrap();
    assert_eq!(loaded["bead-rs"].binary, "custom-bead");
    assert_eq!(loaded["bead-rs"].verified_against, "custom build");
    assert!(loaded.contains_key("bead-forge"));
}

#[test]
fn unknown_strategy_error_names_file_and_operation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unknown-strategy.yaml");
    let mut backend = builtin_bead_backends().remove(0);
    backend.operations.get_mut("claim").unwrap().strategy = Some("teleport".to_string());
    write_descriptor(&path, &backend);

    let error = load_bead_backends(directory.path(), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("operation 'claim'"));
    assert!(error.contains("strategy 'teleport'"));
}

#[test]
fn unresolved_placeholder_error_names_file_and_operation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unknown-placeholder.yaml");
    let mut backend = builtin_bead_backends().remove(0);
    backend
        .operations
        .get_mut("show")
        .unwrap()
        .argv
        .push("{mystery}".to_string());
    write_descriptor(&path, &backend);

    let error = load_bead_backends(directory.path(), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("operation 'show'"));
    assert!(error.contains("placeholder '{mystery}'"));
}

#[test]
fn missing_operation_error_names_file_and_operation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-operation.yaml");
    let mut backend = builtin_bead_backends().remove(0);
    backend.operations.remove("claim_auto");
    write_descriptor(&path, &backend);

    let error = load_bead_backends(directory.path(), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("missing required operation 'claim_auto'"));
}

#[test]
fn absent_identity_pattern_error_names_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-identity.yaml");
    let mut backend = builtin_bead_backends().remove(0);
    backend.identity_pattern.clear();
    write_descriptor(&path, &backend);

    let error = load_bead_backends(directory.path(), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("missing identity_pattern"));
}

#[test]
fn non_yaml_files_are_ignored_and_missing_directory_uses_builtins() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("README.txt"), "not yaml").unwrap();
    let builtins = builtin_bead_backends();
    assert_eq!(
        load_bead_backends(directory.path(), &builtins)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        load_bead_backends(&directory.path().join("absent"), &builtins)
            .unwrap()
            .len(),
        2
    );
}
