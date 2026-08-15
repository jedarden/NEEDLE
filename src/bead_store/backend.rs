//! Data-driven bead CLI backend descriptors.
//!
//! Built-ins and user YAML share the same representation. Runtime consumption
//! is introduced separately; this module owns loading and fail-fast validation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::validate_strategy_name;

const REQUIRED_OPERATIONS: &[&str] = &[
    "ready",
    "list_all",
    "show",
    "claim",
    "claim_auto",
    "release",
    "block",
    "clear_assignee",
    "flush",
    "reopen",
    "labels",
    "label_add",
    "label_remove",
    "create",
    "create_id",
    "dep_add",
    "split",
    "dep_remove",
    "close",
    "doctor_check",
    "doctor_repair",
    "import",
];

/// Parse shape expected from one CLI operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseShape {
    None,
    BareId,
    JsonObject,
    JsonArray,
    JsonLines,
}

/// One operation supplied by a backend descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadOperationSpec {
    /// Argument vector after the descriptor's binary name.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Structural strategy selected for this operation, when applicable.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Output parser used by the generic CLI store.
    #[serde(default)]
    pub parse: Option<ParseShape>,
    /// Per-operation timeout override; zero is rejected.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Capabilities whose absence changes NEEDLE's safety guarantees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadBackendCapabilities {
    #[serde(default)]
    pub atomic_claim: bool,
    #[serde(default)]
    pub transactional_batch: bool,
    #[serde(default)]
    pub velocity_metadata: bool,
}

/// Version-scoped workaround declared by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadBackendQuirk {
    pub name: String,
    #[serde(default)]
    pub version_requirement: Option<String>,
    pub description: String,
}

/// Error fragments used to classify backend failures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadBackendErrorMarkers {
    #[serde(default)]
    pub corruption: Vec<String>,
    #[serde(default)]
    pub lock: Vec<String>,
    #[serde(default)]
    pub sync_conflict: Vec<String>,
}

/// A complete bead CLI dialect described as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadBackend {
    pub name: String,
    pub binary: String,
    #[serde(default)]
    pub detect_paths: Vec<PathBuf>,
    pub identity_pattern: String,
    #[serde(default = "default_version_command")]
    pub version_command: Vec<String>,
    pub verified_against: String,
    pub verified_on: String,
    pub operations: HashMap<String, BeadOperationSpec>,
    #[serde(default)]
    pub capabilities: BeadBackendCapabilities,
    #[serde(default)]
    pub quirks: Vec<BeadBackendQuirk>,
    #[serde(default)]
    pub error_markers: BeadBackendErrorMarkers,
}

fn default_version_command() -> Vec<String> {
    vec!["--version".to_string()]
}

impl BeadBackend {
    /// Classify an error using only this backend's declared markers.
    pub fn error_contains_any(&self, message: &str, markers: &[String]) -> bool {
        let message = message.to_lowercase();
        markers
            .iter()
            .any(|marker| message.contains(&marker.to_lowercase()))
    }

    /// Validate everything that could otherwise fail during the first claim.
    pub fn validate(&self, source: &Path) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("descriptor {} has an empty backend name", source.display());
        }
        if self.binary.trim().is_empty() {
            bail!(
                "descriptor {} for backend '{}' has an empty binary",
                source.display(),
                self.name
            );
        }
        if self.identity_pattern.trim().is_empty() {
            bail!(
                "descriptor {} for backend '{}' is missing identity_pattern",
                source.display(),
                self.name
            );
        }
        Regex::new(&self.identity_pattern).with_context(|| {
            format!(
                "descriptor {} for backend '{}' has invalid identity_pattern {:?}",
                source.display(),
                self.name,
                self.identity_pattern
            )
        })?;
        if self.version_command.is_empty() {
            bail!(
                "descriptor {} for backend '{}' has an empty version_command",
                source.display(),
                self.name
            );
        }

        for required in REQUIRED_OPERATIONS {
            if !self.operations.contains_key(*required) {
                bail!(
                    "descriptor {} for backend '{}' is missing required operation '{}'",
                    source.display(),
                    self.name,
                    required
                );
            }
        }

        for (operation, spec) in &self.operations {
            if spec.timeout_secs == Some(0) {
                bail!(
                    "descriptor {} operation '{}' has a zero timeout",
                    source.display(),
                    operation
                );
            }
            if let Some(strategy) = &spec.strategy {
                validate_strategy_name(source, operation, strategy)?;
            }
            validate_placeholders(source, operation, &spec.argv)?;
        }
        Ok(())
    }
}

fn validate_placeholders(source: &Path, operation: &str, argv: &[String]) -> Result<()> {
    let placeholder = Regex::new(r"\{([^{}]+)\}").expect("static placeholder regex is valid");
    let allowed = allowed_placeholders(operation);

    for argument in argv {
        let mut remainder = argument.clone();
        for capture in placeholder.captures_iter(argument) {
            let full = capture.get(0).expect("capture zero exists").as_str();
            let name = capture.get(1).expect("capture one exists").as_str();
            if !allowed.contains(&name) {
                bail!(
                    "descriptor {} operation '{}' has unresolvable placeholder '{{{}}}'",
                    source.display(),
                    operation,
                    name
                );
            }
            remainder = remainder.replacen(full, "", 1);
        }
        if remainder.contains('{') || remainder.contains('}') {
            bail!(
                "descriptor {} operation '{}' has malformed placeholder in {:?}",
                source.display(),
                operation,
                argument
            );
        }
    }
    Ok(())
}

fn allowed_placeholders(operation: &str) -> &'static [&'static str] {
    match operation {
        "ready" => &["limit", "assignee"],
        "list_all" => &["limit"],
        "show" | "release" | "block" | "clear_assignee" | "reopen" | "labels" => &["id"],
        "claim" => &["id", "actor"],
        "claim_auto" => &["actor", "model", "harness", "harness_version"],
        "label_add" | "label_remove" => &["id", "label"],
        "create" => &[
            "title",
            "body",
            "priority",
            "assignee",
            "issue_type",
            "labels",
        ],
        "dep_add" | "dep_remove" => &["blocked", "blocker"],
        "split" => &["parent", "children"],
        "close" => &["id", "reason"],
        "import" => &["input", "mode", "actor"],
        "flush" | "doctor_check" | "doctor_repair" | "create_id" => &[],
        _ => &[],
    }
}

/// Load built-ins plus user YAML descriptors, overriding built-ins by name.
pub fn load_bead_backends(
    dir: &Path,
    built_ins: &[BeadBackend],
) -> Result<HashMap<String, BeadBackend>> {
    let mut backends = HashMap::new();
    for backend in built_ins {
        let source = PathBuf::from(format!("<builtin:{}>", backend.name));
        backend.validate(&source)?;
        backends.insert(backend.name.clone(), backend.clone());
    }

    if !dir.exists() {
        return Ok(backends);
    }
    if !dir.is_dir() {
        bail!(
            "bead backend descriptor path is not a directory: {}",
            dir.display()
        );
    }

    let mut paths = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read bead backend directory: {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();

    for path in paths {
        let is_yaml = path
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml");
        if !is_yaml {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read bead backend file: {}", path.display()))?;
        let backend: BeadBackend = serde_yaml::from_str(&text)
            .with_context(|| format!("invalid YAML in bead backend file: {}", path.display()))?;
        backend.validate(&path)?;
        backends.insert(backend.name.clone(), backend);
    }
    Ok(backends)
}

/// Shipped descriptors. User files can replace either descriptor by name.
pub fn builtin_bead_backends() -> Vec<BeadBackend> {
    vec![builtin_bead_rs(), builtin_bead_forge()]
}

fn operation(
    argv: &[&str],
    strategy: Option<&str>,
    parse: Option<ParseShape>,
) -> BeadOperationSpec {
    BeadOperationSpec {
        argv: argv.iter().map(|value| (*value).to_string()).collect(),
        strategy: strategy.map(ToOwned::to_owned),
        parse,
        timeout_secs: None,
    }
}

fn common_operations() -> HashMap<String, BeadOperationSpec> {
    [
        ("ready", operation(&[], None, Some(ParseShape::JsonLines))),
        (
            "list_all",
            operation(&[], None, Some(ParseShape::JsonLines)),
        ),
        ("show", operation(&[], None, Some(ParseShape::JsonObject))),
        ("claim", operation(&[], None, Some(ParseShape::JsonObject))),
        (
            "claim_auto",
            operation(&[], None, Some(ParseShape::JsonObject)),
        ),
        ("release", operation(&[], None, None)),
        ("block", operation(&[], None, None)),
        ("clear_assignee", operation(&[], None, None)),
        ("flush", operation(&[], None, None)),
        ("reopen", operation(&[], None, None)),
        ("labels", operation(&[], None, None)),
        ("label_add", operation(&[], None, None)),
        ("label_remove", operation(&[], None, None)),
        ("create", operation(&[], None, Some(ParseShape::BareId))),
        ("create_id", operation(&[], Some("bare_id"), None)),
        ("dep_add", operation(&[], None, None)),
        ("split", operation(&[], None, None)),
        ("dep_remove", operation(&[], None, None)),
        ("close", operation(&[], None, None)),
        ("doctor_check", operation(&[], None, None)),
        ("doctor_repair", operation(&[], None, None)),
        ("import", operation(&[], None, None)),
    ]
    .into_iter()
    .map(|(name, spec)| (name.to_string(), spec))
    .collect()
}

fn builtin_bead_rs() -> BeadBackend {
    let mut operations = common_operations();
    operations.insert(
        "ready".into(),
        operation(
            &["list", "--ready", "--json", "--limit", "{limit}"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "list_all".into(),
        operation(
            &["list", "--json", "--limit", "{limit}"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "show".into(),
        operation(
            &["show", "{id}", "--json"],
            None,
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "claim".into(),
        operation(
            &[
                "update",
                "{id}",
                "--status",
                "in_progress",
                "--assignee",
                "{actor}",
            ],
            Some("compare_and_set"),
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "claim_auto".into(),
        operation(
            &["claim", "--assignee", "{actor}", "--json"],
            Some("atomic_subcommand"),
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "release".into(),
        operation(&["release", "{id}"], None, None),
    );
    operations.insert(
        "block".into(),
        operation(&["update", "{id}", "--status", "blocked"], None, None),
    );
    operations.insert(
        "clear_assignee".into(),
        operation(&["update", "{id}", "--clear-assignee"], None, None),
    );
    operations.insert(
        "flush".into(),
        operation(&["sync", "flush-only"], None, None),
    );
    operations.insert("reopen".into(), operation(&["reopen", "{id}"], None, None));
    operations.insert("labels".into(), operation(&[], Some("repeated"), None));
    operations.insert(
        "label_add".into(),
        operation(&["label", "add", "{id}", "--label", "{label}"], None, None),
    );
    operations.insert(
        "label_remove".into(),
        operation(
            &["label", "remove", "{id}", "--label", "{label}"],
            None,
            None,
        ),
    );
    operations.insert(
        "create".into(),
        operation(
            &["create", "--title", "{title}", "--description", "{body}"],
            None,
            Some(ParseShape::BareId),
        ),
    );
    operations.insert(
        "dep_add".into(),
        operation(
            &["dep", "add", "{blocked}", "{blocker}", "--kind", "blocks"],
            None,
            None,
        ),
    );
    operations.insert("split".into(), operation(&[], Some("sequential"), None));
    operations.insert(
        "dep_remove".into(),
        operation(&["dep", "remove", "{blocked}", "{blocker}"], None, None),
    );
    operations.insert(
        "close".into(),
        operation(&["close", "{id}", "--reason", "{reason}"], None, None),
    );
    operations.insert("doctor_check".into(), operation(&["doctor"], None, None));
    operations.insert(
        "doctor_repair".into(),
        operation(&["doctor", "--repair"], None, None),
    );
    operations.insert(
        "import".into(),
        operation(&["sync", "import-only"], Some("input_plus_mode"), None),
    );

    BeadBackend {
        name: "bead-rs".to_string(),
        binary: "bead".to_string(),
        detect_paths: vec![
            PathBuf::from("~/.cargo/bin/bead"),
            PathBuf::from("~/.local/bin/bead"),
            PathBuf::from("/usr/local/cargo/bin/bead"),
        ],
        identity_pattern: r"^bead\s".to_string(),
        version_command: default_version_command(),
        verified_against: "bead 0.1.3 (commit 85f36ac)".to_string(),
        verified_on: "2026-08-13".to_string(),
        operations,
        capabilities: BeadBackendCapabilities {
            atomic_claim: true,
            transactional_batch: false,
            velocity_metadata: false,
        },
        quirks: Vec::new(),
        error_markers: BeadBackendErrorMarkers {
            corruption: vec![
                "database disk image is malformed".to_string(),
                "database or disk is full".to_string(),
                "attempt to write a readonly database".to_string(),
                "file is not a database".to_string(),
            ],
            lock: vec![
                "database is locked".to_string(),
                "sqlite error: 5".to_string(),
                "sqlite error: 6".to_string(),
            ],
            sync_conflict: Vec::new(),
        },
    }
}

fn builtin_bead_forge() -> BeadBackend {
    let mut backend = builtin_bead_rs();
    backend.name = "bead-forge".to_string();
    backend.binary = "bf".to_string();
    backend.detect_paths = vec![PathBuf::from("~/.local/bin/bf")];
    backend.identity_pattern = r"^bf\s".to_string();
    backend.verified_against = "bf 0.4.1".to_string();
    backend.capabilities = BeadBackendCapabilities {
        atomic_claim: true,
        transactional_batch: true,
        velocity_metadata: true,
    };
    backend.error_markers.sync_conflict = vec![
        "SYNC_CONFLICT".to_string(),
        "JSONL is newer".to_string(),
        "sync conflict".to_string(),
    ];

    let operations = &mut backend.operations;
    operations.insert(
        "ready".into(),
        operation(
            &["ready", "--json", "--limit", "{limit}"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "list_all".into(),
        operation(
            &["list", "--json", "--limit", "{limit}"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "claim".into(),
        operation(&[], Some("batch_op"), Some(ParseShape::JsonObject)),
    );
    operations.insert(
        "claim_auto".into(),
        operation(
            &[
                "claim",
                "--model",
                "{model}",
                "--harness",
                "{harness}",
                "--harness-version",
                "{harness_version}",
                "--assignee",
                "{actor}",
                "--json",
            ],
            Some("atomic_subcommand"),
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert("release".into(), operation(&[], None, None));
    operations.insert("clear_assignee".into(), operation(&[], None, None));
    operations.insert(
        "dep_add".into(),
        operation(
            &["dep", "add", "{blocker}", "--blocks", "{blocked}"],
            None,
            None,
        ),
    );
    operations.insert(
        "split".into(),
        operation(&[], Some("transactional_batch"), None),
    );
    operations.insert(
        "dep_remove".into(),
        operation(&["dep", "remove", "{blocked}", "{blocker}"], None, None),
    );
    operations.insert(
        "import".into(),
        operation(&["sync", "--import-only"], Some("bare"), None),
    );
    backend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_classify_errors_per_descriptor() {
        let bead_rs = builtin_bead_rs();
        let bead_forge = builtin_bead_forge();

        assert!(bead_rs.error_contains_any(
            "Error: database disk image is malformed",
            &bead_rs.error_markers.corruption
        ));
        assert!(!bead_rs.error_contains_any(
            "SYNC_CONFLICT detected",
            &bead_rs.error_markers.sync_conflict
        ));
        assert!(bead_forge.error_contains_any(
            "SYNC_CONFLICT detected",
            &bead_forge.error_markers.sync_conflict
        ));
    }
}
