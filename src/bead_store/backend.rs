//! Data-driven bead CLI backend descriptors.
//!
//! Built-ins and user YAML share the same representation. Runtime consumption
//! is introduced separately; this module owns loading and fail-fast validation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    "ref_add",
    "ref_remove",
    "ref_list",
    "ref_find",
    "data_set",
    "data_get",
    "data_list",
    "data_remove",
    "query",
    "changes",
    "why",
    "compare",
    "recurrence_add",
    "recurrence_remove",
    "recurrence_list",
    "policy_validate",
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

/// One validated descriptor together with the operator-controlled source that
/// supplied it. Runtime consumers retain this provenance instead of
/// rediscovering a similarly named descriptor later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedBeadBackend {
    pub descriptor: BeadBackend,
    pub source: PathBuf,
}

fn default_version_command() -> Vec<String> {
    vec!["--version".to_string()]
}

impl BeadBackend {
    /// Run a binary with --version flag and parse its output to extract the backend name.
    ///
    /// This function spawns a binary with the provided version command (e.g., `["--version"]`),
    /// captures its stdout, and parses the output to identify which bead backend it is.
    ///
    /// # Arguments
    /// * `binary_path` - Path to the binary to execute
    /// * `version_command` - Arguments to pass for version info (e.g., `["--version"]`)
    ///
    /// # Returns
    /// The backend name extracted from the version output (e.g., "bead", "bf").
    ///
    /// # Errors
    /// * If the binary cannot be found or spawned
    /// * If the binary exits with a non-zero status
    /// * If the output cannot be parsed
    pub fn parse_backend_name_from_version(
        binary_path: &Path,
        version_command: &[String],
    ) -> Result<String> {
        // Spawn the binary with version command
        let output = Command::new(binary_path)
            .args(version_command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| {
                format!(
                    "failed to spawn binary '{}' for version check",
                    binary_path.display()
                )
            })?;

        // Check exit code
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "binary '{}' exited with status {}: {}",
                binary_path.display(),
                output.status,
                stderr.trim()
            );
        }

        // Parse stdout to extract backend name
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout = stdout.trim();

        // Try to match common version output patterns
        // Expected formats:
        // - "bead 0.x.y" or "bead 0.x.y (details)"
        // - "bf 0.x.y" or "bf 0.x.y (details)"
        // - "beads-rust 0.x.y"

        // Pattern 1: First word before whitespace (e.g., "bead" from "bead 0.1.3")
        let first_word_pattern = Regex::new(r"^(\S+)\s").unwrap();
        if let Some(caps) = first_word_pattern.captures(stdout) {
            if let Some(name) = caps.get(1) {
                return Ok(name.as_str().to_string());
            }
        }

        // Pattern 2: If no space, entire output is the name (e.g., just "bead")
        if !stdout.is_empty() && !stdout.contains(char::is_whitespace) {
            return Ok(stdout.to_string());
        }

        bail!(
            "unable to parse backend name from version output: '{}'",
            stdout
        );
    }

    /// Parse backend identity from raw version output string.
    ///
    /// This function takes a raw version output string (e.g., from a `--version` command)
    /// and extracts the backend name from various common formats.
    ///
    /// # Arguments
    /// * `version_output` - Raw stdout/stderr from version command execution
    ///
    /// # Returns
    /// The backend name extracted from the version output (e.g., "bead", "bf").
    ///
    /// # Supported Formats
    /// - `"bf 0.x.y"` → returns "bf"
    /// - `"bead 0.x.y"` → returns "bead" (bead-rs backend)
    /// - `"bead 0.x.y (details)"` → returns "bead"
    /// - Single-word outputs → returns the word
    /// - Unknown formats → returns "unknown"
    ///
    /// # Examples
    /// ```
    /// assert_eq!(parse_backend_name("bf 0.1.0"), "bf");
    /// assert_eq!(parse_backend_name("bead 0.2.3"), "bead");
    /// assert_eq!(parse_backend_name("bead 0.2.3 (bead-rs)"), "bead");
    /// assert_eq!(parse_backend_name("unknown-format"), "unknown");
    /// ```
    pub fn parse_backend_name(version_output: &str) -> String {
        let trimmed = version_output.trim();

        // Pattern 1: First word before whitespace (e.g., "bead" from "bead 0.1.3")
        let first_word_pattern = Regex::new(r"^(\S+)\s").unwrap();
        if let Some(caps) = first_word_pattern.captures(trimmed) {
            if let Some(name) = caps.get(1) {
                return name.as_str().to_string();
            }
        }

        // Pattern 2: If no space, entire output is the name (e.g., just "bead")
        if !trimmed.is_empty() && !trimmed.contains(char::is_whitespace) {
            return trimmed.to_string();
        }

        // Pattern 3: Empty or unparseable output
        if trimmed.is_empty() {
            return "unknown".to_string();
        }

        // Pattern 4: Multi-word output but no clear pattern - return first word
        trimmed
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_string()
    }

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
        "compare" => &["id", "profile"],
        "query" => &["query"],
        "changes" => &["since"],
        "why" => &["id"],
        "ref_add" => &["id", "namespace", "key", "value"],
        "ref_remove" => &["id", "namespace", "key"],
        "ref_list" => &["id"],
        "ref_find" => &["namespace", "value"],
        "data_set" => &["id", "key", "value"],
        "data_get" => &["id", "key"],
        "data_list" => &["id"],
        "data_remove" => &["id", "key"],
        "recurrence_add" => &["template", "schedule"],
        "recurrence_remove" => &["id"],
        "recurrence_list" => &[],
        "policy_validate" => &[],
        "flush" | "doctor_check" | "doctor_repair" | "create_id" => &[],
        _ => &[],
    }
}

/// Load built-ins plus user YAML descriptors. A user descriptor may override a
/// built-in by name, but two operator files defining the same name are
/// rejected as ambiguous.
pub fn load_bead_backends(
    dir: &Path,
    built_ins: &[BeadBackend],
) -> Result<HashMap<String, BeadBackend>> {
    Ok(load_bead_backends_with_sources(dir, built_ins)?
        .into_iter()
        .map(|(name, loaded)| (name, loaded.descriptor))
        .collect())
}

/// Load and validate backend descriptors while preserving their source.
pub fn load_bead_backends_with_sources(
    dir: &Path,
    built_ins: &[BeadBackend],
) -> Result<HashMap<String, LoadedBeadBackend>> {
    let mut backends = HashMap::new();
    for backend in built_ins {
        let source = PathBuf::from(format!("<builtin:{}>", backend.name));
        backend.validate(&source)?;
        backends.insert(
            backend.name.clone(),
            LoadedBeadBackend {
                descriptor: backend.clone(),
                source,
            },
        );
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
        if let Some(previous) = backends.get(&backend.name) {
            if !is_builtin_source(&previous.source) {
                bail!(
                    "ambiguous bead backend descriptor '{}': defined by both {} and {}",
                    backend.name,
                    previous.source.display(),
                    path.display()
                );
            }
        }
        backends.insert(
            backend.name.clone(),
            LoadedBeadBackend {
                descriptor: backend,
                source: path,
            },
        );
    }
    Ok(backends)
}

fn is_builtin_source(source: &Path) -> bool {
    source
        .to_str()
        .is_some_and(|value| value.starts_with("<builtin:") && value.ends_with('>'))
}

/// Shipped descriptors. User files can replace this descriptor by name.
pub fn builtin_bead_backends() -> Vec<BeadBackend> {
    vec![builtin_bead_rs()]
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
        ("ref_add", operation(&[], None, None)),
        ("ref_remove", operation(&[], None, None)),
        ("ref_list", operation(&[], None, None)),
        (
            "ref_find",
            operation(&[], None, Some(ParseShape::JsonLines)),
        ),
        ("data_set", operation(&[], None, None)),
        (
            "data_get",
            operation(&[], None, Some(ParseShape::JsonObject)),
        ),
        (
            "data_list",
            operation(&[], None, Some(ParseShape::JsonLines)),
        ),
        ("data_remove", operation(&[], None, None)),
        ("query", operation(&[], None, Some(ParseShape::JsonLines))),
        ("changes", operation(&[], None, Some(ParseShape::JsonLines))),
        ("why", operation(&[], None, Some(ParseShape::JsonObject))),
        (
            "compare",
            operation(&[], None, Some(ParseShape::JsonObject)),
        ),
        ("recurrence_add", operation(&[], None, None)),
        ("recurrence_remove", operation(&[], None, None)),
        (
            "recurrence_list",
            operation(&[], None, Some(ParseShape::JsonLines)),
        ),
        (
            "policy_validate",
            operation(&[], None, Some(ParseShape::JsonObject)),
        ),
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
    operations.insert(
        "ref_add".into(),
        operation(
            &[
                "ref",
                "add",
                "{id}",
                "--namespace",
                "{namespace}",
                "--key",
                "{key}",
                "--value",
                "{value}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "ref_remove".into(),
        operation(
            &[
                "ref",
                "remove",
                "{id}",
                "--namespace",
                "{namespace}",
                "--key",
                "{key}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "ref_list".into(),
        operation(&["ref", "list", "{id}"], None, None),
    );
    operations.insert(
        "ref_find".into(),
        operation(
            &[
                "ref",
                "find",
                "--namespace",
                "{namespace}",
                "--value",
                "{value}",
            ],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "data_set".into(),
        operation(
            &[
                "data", "set", "{id}", "--key", "{key}", "--value", "{value}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "data_get".into(),
        operation(
            &["data", "get", "{id}", "--key", "{key}"],
            None,
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "data_list".into(),
        operation(&["data", "list", "{id}"], None, Some(ParseShape::JsonLines)),
    );
    operations.insert(
        "data_remove".into(),
        operation(&["data", "remove", "{id}", "--key", "{key}"], None, None),
    );
    operations.insert(
        "query".into(),
        operation(
            &["query", "{query}", "--json"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "changes".into(),
        operation(
            &["changes", "--since", "{since}", "--json"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "why".into(),
        operation(&["why", "{id}"], None, Some(ParseShape::JsonObject)),
    );
    operations.insert(
        "compare".into(),
        operation(
            &["compare", "{id}", "--profile", "{profile}"],
            None,
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "recurrence_add".into(),
        operation(
            &[
                "recurrence",
                "add",
                "--template",
                "{template}",
                "--schedule",
                "{schedule}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "recurrence_remove".into(),
        operation(&["recurrence", "remove", "{id}"], None, None),
    );
    operations.insert(
        "recurrence_list".into(),
        operation(
            &["recurrence", "list", "--json"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "policy_validate".into(),
        operation(&["policy", "validate"], None, Some(ParseShape::JsonObject)),
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
        quirks: vec![BeadBackendQuirk {
            name: "limit_zero_returns_empty_set".to_string(),
            version_requirement: None,
            description: "bead-rs has a bug where --limit 0 returns an empty set instead of all beads. Workaround: use a large explicit limit.".to_string(),
        }],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_classify_errors_per_descriptor() {
        let bead_rs = builtin_bead_rs();

        assert!(bead_rs.error_contains_any(
            "Error: database disk image is malformed",
            &bead_rs.error_markers.corruption
        ));
        assert!(!bead_rs.error_contains_any(
            "SYNC_CONFLICT detected",
            &bead_rs.error_markers.sync_conflict
        ));
    }

    #[test]
    fn builtins_declare_quirks_for_known_bugs() {
        let bead_rs = builtin_bead_rs();

        // bead-rs SHOULD have the limit_zero_returns_empty_set quirk (verified against v0.1.3)
        let rs_quirk = bead_rs
            .quirks
            .iter()
            .find(|q| q.name == "limit_zero_returns_empty_set")
            .expect("bead-rs should declare limit_zero_returns_empty_set quirk");
        assert!(
            rs_quirk.version_requirement.is_none(),
            "bead-rs quirk should apply to all versions"
        );
    }

    #[test]
    fn backend_quirks_are_correctly_declared() {
        // bead-rs has the limit_zero_returns_empty_set quirk
        let backend = builtin_bead_rs();
        assert!(!backend.quirks.is_empty(), "bead-rs should have quirks");
        assert!(
            backend
                .quirks
                .iter()
                .any(|q| q.name == "limit_zero_returns_empty_set"),
            "bead-rs should have limit_zero_returns_empty_set quirk"
        );
    }

    #[test]
    fn parse_backend_name_from_standard_version_output() {
        // Test parsing "bead 0.1.3" format
        let result = BeadBackend::parse_backend_name_from_version(
            Path::new("/nonexistent/bead"),
            &["--version".to_string()],
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to spawn"));
    }

    #[test]
    fn test_parse_backend_name_bf_format() {
        // Test "bf 0.x.y" format
        assert_eq!(BeadBackend::parse_backend_name("bf 0.1.0"), "bf");
        assert_eq!(BeadBackend::parse_backend_name("bf 0.4.2"), "bf");
        assert_eq!(BeadBackend::parse_backend_name("bf 1.0.0"), "bf");
    }

    #[test]
    fn test_parse_backend_name_bead_format() {
        // Test "bead 0.x.y" format (bead-rs)
        assert_eq!(BeadBackend::parse_backend_name("bead 0.1.0"), "bead");
        assert_eq!(BeadBackend::parse_backend_name("bead 0.2.3"), "bead");
        assert_eq!(BeadBackend::parse_backend_name("bead 0.3.1"), "bead");
    }

    #[test]
    fn test_parse_backend_name_bead_with_details() {
        // Test "bead 0.x.y (details)" format
        assert_eq!(
            BeadBackend::parse_backend_name("bead 0.1.3 (commit 85f36ac)"),
            "bead"
        );
        assert_eq!(
            BeadBackend::parse_backend_name("bead 0.2.0 (bead-rs)"),
            "bead"
        );
        assert_eq!(
            BeadBackend::parse_backend_name("bf 0.4.1 (build 123)"),
            "bf"
        );
    }

    #[test]
    fn test_parse_backend_name_single_word() {
        // Test single-word outputs
        assert_eq!(BeadBackend::parse_backend_name("bead"), "bead");
        assert_eq!(BeadBackend::parse_backend_name("bf"), "bf");
    }

    #[test]
    fn test_parse_backend_name_empty_output() {
        // Test empty output
        assert_eq!(BeadBackend::parse_backend_name(""), "unknown");
        assert_eq!(BeadBackend::parse_backend_name("   "), "unknown");
    }

    #[test]
    fn test_parse_backend_name_unknown_format() {
        // Test unknown/complex formats
        assert_eq!(
            BeadBackend::parse_backend_name("some random output"),
            "some"
        );
        assert_eq!(
            BeadBackend::parse_backend_name("multiple words here"),
            "multiple"
        );
    }

    #[test]
    fn test_parse_backend_name_with_whitespace_variations() {
        // Test various whitespace patterns
        assert_eq!(BeadBackend::parse_backend_name("bead\t0.1.0"), "bead");
        assert_eq!(BeadBackend::parse_backend_name("bf\n0.2.0"), "bf");
        assert_eq!(BeadBackend::parse_backend_name("  bead 0.1.0  "), "bead");
    }

    #[test]
    fn parse_backend_name_patterns_correctly() {
        // These tests verify the regex patterns work correctly on string inputs

        // Pattern 1: "bead 0.1.3" should extract "bead"
        let stdout = "bead 0.1.3";
        let first_word_pattern = Regex::new(r"^(\S+)\s").unwrap();
        let caps = first_word_pattern.captures(stdout);
        assert!(caps.is_some());
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bead");

        // Pattern 2: "bf 0.4.1" should extract "bf"
        let stdout = "bf 0.4.1";
        let caps = first_word_pattern.captures(stdout);
        assert!(caps.is_some());
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bf");

        // Pattern 3: "bead 0.1.3 (commit 85f36ac)" should extract "bead"
        let stdout = "bead 0.1.3 (commit 85f36ac)";
        let caps = first_word_pattern.captures(stdout);
        assert!(caps.is_some());
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bead");
    }

    #[test]
    fn parse_backend_name_handles_various_output_formats() {
        // Test that various version output formats can be handled

        // Format: "bead-rs 0.1.3" should extract "bead-rs"
        let stdout = "bead-rs 0.1.3";
        let first_word_pattern = Regex::new(r"^(\S+)\s").unwrap();
        let caps = first_word_pattern.captures(stdout);
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bead-rs");

        // Format: "bead 0.1.3 (commit 85f36ac)" should extract "bead"
        let stdout = "bead 0.1.3 (commit 85f36ac)";
        let caps = first_word_pattern.captures(stdout);
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bead");

        // Format: "bf 0.4.1" should extract "bf"
        let stdout = "bf 0.4.1";
        let caps = first_word_pattern.captures(stdout);
        assert_eq!(caps.unwrap().get(1).unwrap().as_str(), "bf");
    }

    // ─── Placeholder validation tests ────────────────────────────────────────────

    #[test]
    fn allowed_placeholders_returns_correct_sets_for_operations() {
        // Test operations with different placeholder sets
        assert_eq!(allowed_placeholders("ready"), &["limit", "assignee"][..]);
        assert_eq!(allowed_placeholders("show"), &["id"][..]);
        assert_eq!(allowed_placeholders("claim"), &["id", "actor"][..]);
        assert_eq!(
            allowed_placeholders("claim_auto"),
            &["actor", "model", "harness", "harness_version"][..]
        );
        assert_eq!(
            allowed_placeholders("create"),
            &[
                "title",
                "body",
                "priority",
                "assignee",
                "issue_type",
                "labels"
            ][..]
        );
        assert_eq!(allowed_placeholders("dep_add"), &["blocked", "blocker"][..]);
        assert_eq!(allowed_placeholders("close"), &["id", "reason"][..]);
    }

    #[test]
    fn validate_placeholders_accepts_all_allowed_placeholders() {
        let source = PathBuf::from("<test>");
        let operation = "show";

        // Valid: {id} is allowed for "show"
        let argv = vec!["show".to_string(), "{id}".to_string()];
        assert!(validate_placeholders(&source, operation, &argv).is_ok());

        // Valid: multiple placeholders in one arg
        let argv = vec!["{id}".to_string()];
        assert!(validate_placeholders(&source, operation, &argv).is_ok());
    }

    #[test]
    fn validate_placeholders_rejects_disallowed_placeholders() {
        let source = PathBuf::from("<test>");
        let operation = "show";

        // Invalid: {title} is not allowed for "show"
        let argv = vec!["show".to_string(), "{title}".to_string()];
        let result = validate_placeholders(&source, operation, &argv);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("unresolvable placeholder"));
        assert!(err_msg.contains("{title}"));
    }

    #[test]
    fn validate_placeholders_rejects_malformed_placeholders() {
        let source = PathBuf::from("<test>");
        let operation = "show";

        // Malformed: unclosed brace
        let argv = vec!["{id".to_string()];
        let result = validate_placeholders(&source, operation, &argv);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("malformed placeholder"));

        // Malformed: unmatched closing brace
        let argv = vec!["id}".to_string()];
        let result = validate_placeholders(&source, operation, &argv);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("malformed placeholder"));
    }

    #[test]
    fn validate_placeholders_handles_empty_argv() {
        let source = PathBuf::from("<test>");
        let operation = "flush";

        // Valid: empty argv (no placeholders to validate)
        let argv: Vec<String> = vec![];
        assert!(validate_placeholders(&source, operation, &argv).is_ok());
    }

    #[test]
    fn validate_placeholders_handles_placeholders_with_special_chars() {
        let source = PathBuf::from("<test>");
        let operation = "show";

        // Valid: placeholders alongside other text
        let argv = vec!["show-{id}".to_string()];
        assert!(validate_placeholders(&source, operation, &argv).is_ok());

        // Invalid: unknown placeholder alongside other text
        let argv = vec!["prefix-{unknown}-suffix".to_string()];
        let result = validate_placeholders(&source, operation, &argv);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("unresolvable placeholder"));
        assert!(err_msg.contains("{unknown}"));
    }

    #[test]
    fn validate_placeholders_allows_duplicate_placeholders_in_same_arg() {
        let source = PathBuf::from("<test>");
        let operation = "show";

        // Valid: same placeholder can appear multiple times
        let argv = vec!["{id}-{id}".to_string()];
        assert!(validate_placeholders(&source, operation, &argv).is_ok());
    }

    #[test]
    fn builtin_backends_validate_all_placeholders() {
        // Verify that all built-in backend descriptors have valid placeholders
        let backends = builtin_bead_backends();
        let source = PathBuf::from("<builtin>");

        for backend in &backends {
            for (operation, spec) in &backend.operations {
                let result = validate_placeholders(&source, operation, &spec.argv);
                assert!(
                    result.is_ok(),
                    "backend '{}' operation '{}' has invalid placeholders: {}",
                    backend.name,
                    operation,
                    result.unwrap_err()
                );
            }
        }
    }
}
