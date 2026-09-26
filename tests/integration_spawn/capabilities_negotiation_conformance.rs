//! Process-boundary conformance tests for the documented bead-rs capability
//! negotiation contract and the worker's final fail-closed gate.

#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore};
use needle::claim::{ClaimIdentity, ResolvedStoreContext};
use needle::config::{BeadBackend as ConfiguredBackend, BeadCliConfig, TransitionsConfig};
use needle::dispatch::{AgentAdapter, DispatchContext, Dispatcher, TokenExtraction};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{BeadId, InputMethod};
use tempfile::TempDir;

fn capability_document() -> serde_json::Value {
    serde_json::json!({
        "implementation": "bead-rs",
        "atomic_claim": true,
        "statuses": ["open", "in_progress", "deferred", "closed"],
        "schemas": [
            {"schema_ref": "urn:bead-rs:schema:issue:native-v1"},
            {"schema_ref": "urn:bead-rs:schema:event:native-v1"},
            {"schema_ref": "urn:bead-rs:schema:field-guide:native-v1"},
        ],
        "commands": ["ref", "data", "query"],
    })
}

fn capability_binary(workspace: &Path, capabilities: &serde_json::Value) -> PathBuf {
    let path = workspace.join("bead");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' 'bead 0.2.6' ;;\n  capabilities) [ \"$2\" = --profile ] && [ \"$3\" = native-v1 ] || exit 9; printf '%s' '{capabilities}' ;;\n  *) exit 8 ;;\nesac\n",
        capabilities = capabilities
    );
    fs::write(&path, script).expect("write mock bead executable");
    let mut permissions = fs::metadata(&path)
        .expect("stat mock bead executable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make mock bead executable runnable");
    path
}

fn open_backend(
    workspace: &Path,
    capabilities: &serde_json::Value,
    transitions: &TransitionsConfig,
) -> anyhow::Result<Arc<dyn BeadStore>> {
    let binary = capability_binary(workspace, capabilities);
    needle::bead_store::open_configured_with_transitions(
        &BeadCliConfig {
            backend: ConfiguredBackend::Bead,
            path: Some(binary),
        },
        workspace.to_path_buf(),
        None,
        None,
        None,
        transitions,
    )
}

pub(super) fn verify_required_identity_status_schema_and_command_capabilities() {
    let valid = capability_document();
    let mut rejected = Vec::<(String, serde_json::Value, String)>::new();

    let mut wrong_implementation = valid.clone();
    wrong_implementation["implementation"] = serde_json::json!("bead-forge");
    rejected.push((
        "wrong implementation".into(),
        wrong_implementation,
        "backend identity mismatch".into(),
    ));

    let mut missing_atomic_claim = valid.clone();
    missing_atomic_claim["atomic_claim"] = serde_json::json!(false);
    rejected.push((
        "atomic claims are not guaranteed".into(),
        missing_atomic_claim,
        "expected atomic_claim=true".into(),
    ));

    for status in ["open", "in_progress", "deferred", "closed"] {
        let mut document = valid.clone();
        document["statuses"] = serde_json::Value::Array(
            document["statuses"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry.as_str() != Some(status))
                .cloned()
                .collect(),
        );
        rejected.push((
            format!("missing status {status}"),
            document,
            format!("missing status {status}"),
        ));
    }

    let mut unexpected_status = valid.clone();
    unexpected_status["statuses"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("blocked"));
    rejected.push((
        "derived blocked status is incompatible".into(),
        unexpected_status,
        "unexpected status 'blocked'".into(),
    ));

    for schema_ref in [
        "urn:bead-rs:schema:issue:native-v1",
        "urn:bead-rs:schema:event:native-v1",
        "urn:bead-rs:schema:field-guide:native-v1",
    ] {
        let mut document = valid.clone();
        document["schemas"] = serde_json::Value::Array(
            document["schemas"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["schema_ref"] != schema_ref)
                .cloned()
                .collect(),
        );
        rejected.push((
            format!("missing schema {schema_ref}"),
            document,
            format!("missing schema {schema_ref}"),
        ));
    }

    for command in ["ref", "data", "query"] {
        let mut document = valid.clone();
        document["commands"] = serde_json::Value::Array(
            document["commands"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry.as_str() != Some(command))
                .cloned()
                .collect(),
        );
        rejected.push((
            format!("missing command {command}"),
            document,
            format!("missing command {command}"),
        ));
    }

    let mut invalid_shape = valid.clone();
    invalid_shape["statuses"] = serde_json::json!("open,in_progress,deferred,closed");
    rejected.push((
        "statuses has an incompatible shape".into(),
        invalid_shape,
        "field 'statuses' is not an array".into(),
    ));

    for (label, document, expected) in rejected {
        let workspace = TempDir::new().unwrap();
        let result = open_backend(workspace.path(), &document, &TransitionsConfig::default());
        let error = result
            .err()
            .unwrap_or_else(|| panic!("{label}: invalid capabilities were accepted"));
        assert!(
            format!("{error:#}").contains(&expected),
            "{label}: expected {expected:?}, got {error:#}"
        );
    }
}

pub(super) fn verify_optional_and_static_capability_projection() {
    let valid = capability_document();
    let workspace = TempDir::new().unwrap();
    let legacy = open_backend(workspace.path(), &valid, &TransitionsConfig::default())
        .expect("base contract opens without optional capabilities");
    let snapshot = legacy
        .negotiated_capabilities()
        .expect("CLI backend publishes its capability snapshot");
    assert_eq!(snapshot["backend"], "bead-rs");
    assert_eq!(snapshot["atomic_claim"], serde_json::json!(true));
    assert_eq!(snapshot["transactional_batch"], serde_json::json!(true));
    assert_eq!(snapshot["velocity_metadata"], serde_json::json!(false));
    assert_eq!(snapshot["manifest"], serde_json::json!(false));
    assert_eq!(snapshot["attempt_resolution"], serde_json::json!(false));

    let mut modern = valid;
    modern["commands"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("manifest"));
    modern["attempt_outcome"] = serde_json::json!({"supported": true});
    let workspace = TempDir::new().unwrap();
    let store = open_backend(workspace.path(), &modern, &TransitionsConfig::default())
        .expect("optional capabilities are accepted");
    let snapshot = store
        .negotiated_capabilities()
        .expect("CLI backend publishes its capability snapshot");
    assert_eq!(snapshot["manifest"], serde_json::json!(true));
    assert_eq!(snapshot["attempt_resolution"], serde_json::json!(true));
}

pub(super) fn verify_each_transition_capability_gate() {
    let transitions = [
        ("attempt_receipt", "attempt"),
        ("atomic_resolution", "resolution"),
        ("durable_facts", "learning"),
        ("claim_fencing", "fenced_claim"),
    ];

    for (capability, name) in transitions {
        let mut enabled = TransitionsConfig::default();
        match name {
            "attempt" => enabled.attempt.enabled = true,
            "resolution" => enabled.resolution.enabled = true,
            "learning" => enabled.learning.enabled = true,
            "fenced_claim" => enabled.fenced_claim.enabled = true,
            _ => unreachable!("transition matrix is exhaustive"),
        }

        let mut advertised = capability_document();
        advertised["transitions"] = serde_json::json!({(capability): true});
        let workspace = TempDir::new().unwrap();
        open_backend(workspace.path(), &advertised, &enabled)
            .unwrap_or_else(|error| panic!("{capability} should enable {name}: {error:#}"));

        let mut absent = capability_document();
        absent["transitions"] = serde_json::json!({});
        let workspace = TempDir::new().unwrap();
        let error = open_backend(workspace.path(), &absent, &enabled)
            .err()
            .unwrap_or_else(|| panic!("{name} must fail without {capability}"));
        assert!(
            format!("{error:#}").contains(&format!("transitions.{name}.enabled")),
            "{name}: unexpected negotiation error: {error:#}"
        );

        let mut false_advertisement = capability_document();
        false_advertisement["transitions"] = serde_json::json!({(capability): false});
        let workspace = TempDir::new().unwrap();
        let error = open_backend(workspace.path(), &false_advertisement, &enabled)
            .err()
            .unwrap_or_else(|| panic!("false {capability} must not enable {name}"));
        assert!(format!("{error:#}").contains(capability));
    }
}

fn worker_adapter() -> AgentAdapter {
    AgentAdapter {
        name: "capability-conformance".into(),
        description: None,
        agent_cli: "test".into(),
        version_command: None,
        input_method: InputMethod::Args {
            flag: "--prompt".into(),
        },
        invoke_template: "echo spawned >> {workspace}/spawned.txt".into(),
        environment: HashMap::new(),
        timeout_secs: 10,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        usage_format: None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

pub(super) async fn verify_worker_blocks_incompatible_capability_probes() {
    let mut missing_atomic_claim = capability_document();
    missing_atomic_claim
        .as_object_mut()
        .unwrap()
        .remove("atomic_claim");
    let mut wrong_implementation = capability_document();
    wrong_implementation["implementation"] = serde_json::json!("bead-forge");
    let cases = [
        ("missing atomic_claim", missing_atomic_claim, "capability"),
        ("wrong implementation", wrong_implementation, "identity"),
    ];

    for (label, document, expected_category) in cases {
        let workspace = TempDir::new().unwrap();
        let binary = capability_binary(workspace.path(), &document);
        let backend = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .expect("built-in bead-rs descriptor exists");
        let store = CliBeadStore::new(
            backend,
            binary,
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();
        let target_store: Arc<dyn BeadStore> = Arc::new(store);

        let telemetry_dir = TempDir::new().unwrap();
        let telemetry = Telemetry::with_log_dir("capability-worker".into(), telemetry_dir.path());
        telemetry.start();
        let adapter = worker_adapter();
        let dispatcher = Dispatcher::with_adapters(
            HashMap::from([(adapter.name.clone(), adapter)]),
            telemetry.clone(),
            3600,
        )
        .with_worker_id("capability-worker".into());
        let context = DispatchContext::new(
            ResolvedStoreContext::new(target_store, workspace.path().to_path_buf()),
            ClaimIdentity {
                actor: "capability-worker".into(),
                revision: Some(7),
                claim_epoch: Some(2),
            },
        );
        let prompt = BuiltPrompt {
            content: "capability probe".into(),
            hash: "capability-probe".into(),
            token_estimate: 4,
            template_name: "capability-conformance".into(),
            template_version: "test-1".into(),
        };

        let result = dispatcher
            .dispatch_with_context(
                &BeadId::from("needle-capability-conformance"),
                &prompt,
                dispatcher.adapter("capability-conformance").unwrap(),
                workspace.path(),
                &context,
            )
            .await;
        assert!(
            result.is_err(),
            "{label}: worker accepted the invalid probe"
        );
        assert!(
            !workspace.path().join("spawned.txt").exists(),
            "{label}: invalid capabilities must block process creation"
        );

        drop(dispatcher);
        telemetry.shutdown().await;
        let events = fs::read_dir(telemetry_dir.path())
            .expect("read worker telemetry directory")
            .flat_map(|entry| {
                let entry = entry.expect("read telemetry entry");
                fs::read_to_string(entry.path())
                    .expect("read telemetry log")
                    .lines()
                    .map(serde_json::from_str::<serde_json::Value>)
                    .collect::<Result<Vec<_>, _>>()
                    .expect("parse telemetry events")
            })
            .collect::<Vec<_>>();
        let failure = events
            .iter()
            .find(|event| event["event_type"] == "bead.claim.verify_error")
            .unwrap_or_else(|| panic!("{label}: expected pre-spawn verification telemetry"));
        assert_eq!(failure["data"]["category"], expected_category);
    }
}
