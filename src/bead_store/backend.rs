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
    "clear_manual_block",
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

/// Inventory and frontier queries whose result sets must never depend on the
/// backend's own default limit (ADR-001 axis 3). A descriptor takes the
/// caller's explicit ceiling through the `{limit}` placeholder, or may
/// hardcode its own positive limit — but an omitted limit falls back to the
/// CLI default, which truncates priority-sorted output until low-priority
/// beads are invisible, and a hardcoded zero has backend-specific meanings
/// including an empty result set. [`BeadBackend::validate`] rejects both
/// shapes at descriptor load so no scan can silently depend on them.
const INVENTORY_OPERATIONS: &[&str] = &["ready", "list_all", "list_in_progress", "manual_blocked"];

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

/// A backend version triple parsed from the binary's `version_command` output
/// (e.g. `0.2.6` out of `bead 0.2.6 (unknown 2026-09-06T14:10:37Z)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BackendVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl BackendVersion {
    /// Extract the first `major.minor.patch` triple from free-form version
    /// output. Non-numeric or missing components yield `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let pattern = Regex::new(r"(\d+)\.(\d+)\.(\d+)").ok()?;
        let captures = pattern.captures(text)?;
        let field = |index: usize| -> Option<u64> {
            captures
                .get(index)
                .and_then(|group| group.as_str().parse().ok())
        };
        Some(Self {
            major: field(1)?,
            minor: field(2)?,
            patch: field(3)?,
        })
    }
}

/// Comparison operator of a quirk's version requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionOp {
    Le,
    Lt,
    Ge,
    Gt,
    Eq,
}

impl VersionOp {
    fn compare(self, version: BackendVersion, bound: BackendVersion) -> bool {
        match self {
            Self::Le => version <= bound,
            Self::Lt => version < bound,
            Self::Ge => version >= bound,
            Self::Gt => version > bound,
            Self::Eq => version == bound,
        }
    }

    fn from_prefix(requirement: &str) -> (Self, &str) {
        // Two-character prefixes first so "<=" is not read as "<" with junk.
        if let Some(rest) = requirement.strip_prefix("<=") {
            (Self::Le, rest)
        } else if let Some(rest) = requirement.strip_prefix(">=") {
            (Self::Ge, rest)
        } else if let Some(rest) = requirement.strip_prefix('<') {
            (Self::Lt, rest)
        } else if let Some(rest) = requirement.strip_prefix('>') {
            (Self::Gt, rest)
        } else if let Some(rest) = requirement.strip_prefix('=') {
            (Self::Eq, rest)
        } else {
            (Self::Eq, requirement)
        }
    }
}

/// Is `text` exactly `MAJOR.MINOR.PATCH`, digits and dots only?
///
/// [`BackendVersion::parse`] deliberately digs a triple out of free-form
/// probe output, where the version is embedded in surrounding text. A
/// quirk requirement is authored, not probed: anything beyond the bare
/// version (`~0.2.6`, `0.2.x`, `0.2`) is a typo and must fail validation
/// instead of silently binding to the digits it happens to contain.
fn is_exact_version(text: &str) -> bool {
    text.split('.').count() == 3
        && text
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Parse a quirk version requirement: one of `<=X.Y.Z`, `<X.Y.Z`, `>=X.Y.Z`,
/// `>X.Y.Z`, `=X.Y.Z`, or a bare `X.Y.Z` (exact). Descriptors are validated
/// with this at load time, so an unparseable range fails fast instead of
/// silently applying or skipping the quirk.
fn parse_version_requirement(requirement: &str) -> Result<(VersionOp, BackendVersion)> {
    let trimmed = requirement.trim();
    let (op, rest) = VersionOp::from_prefix(trimmed);
    let bound = BackendVersion::parse(rest)
        .filter(|_| is_exact_version(rest))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "expected a MAJOR.MINOR.PATCH version (optionally preceded by <=, <, >=, > or =), got {:?}",
                requirement
            )
        })?;
    Ok((op, bound))
}

/// Version-scoped workaround declared by a backend.
///
/// A quirk's `description` is the retirement contract required by ADR-013 §4:
/// it must say WHY the workaround exists, which binary versions exhibit the
/// behaviour, and what evidence lets the range be narrowed or dropped. The
/// workaround itself stays linked to that condition — `applies_to` evaluates
/// the requirement against the running binary's version — instead of being
/// frozen into the call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadBackendQuirk {
    pub name: String,
    #[serde(default)]
    pub version_requirement: Option<String>,
    pub description: String,
}

impl BeadBackendQuirk {
    /// Does this quirk apply to a binary of `runtime_version`?
    ///
    /// A quirk without a version requirement applies unconditionally — declare
    /// one only when every version of the backend needs the workaround. When
    /// the running version is unknown (the descriptor's `version_command`
    /// output could not be parsed), a scoped quirk conservatively applies:
    /// the safe default is the documented-harmless workaround, not the plain
    /// query an old broken binary would answer wrong.
    pub fn applies_to(&self, runtime_version: Option<BackendVersion>) -> bool {
        let Some(requirement) = &self.version_requirement else {
            return true;
        };
        match parse_version_requirement(requirement) {
            Ok((op, bound)) => match runtime_version {
                Some(version) => op.compare(version, bound),
                // Descriptor validation rejects malformed ranges before the
                // store is built; an unparseable range fails safe the same
                // way an unknown version does.
                None => true,
            },
            Err(_) => true,
        }
    }
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

        for inventory_operation in INVENTORY_OPERATIONS {
            if let Some(spec) = self.operations.get(*inventory_operation) {
                validate_inventory_limits(source, inventory_operation, &spec.argv)?;
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

        for quirk in &self.quirks {
            if quirk.name.trim().is_empty() {
                bail!(
                    "descriptor {} for backend '{}' has a quirk with an empty name",
                    source.display(),
                    self.name
                );
            }
            if quirk.description.trim().is_empty() {
                bail!(
                    "descriptor {} for backend '{}' quirk '{}' has an empty description; a quirk must document why it exists and for which versions so it can be retired",
                    source.display(),
                    self.name,
                    quirk.name
                );
            }
            if let Some(requirement) = &quirk.version_requirement {
                parse_version_requirement(requirement).with_context(|| {
                    format!(
                        "descriptor {} for backend '{}' quirk '{}' has invalid version_requirement {:?}",
                        source.display(),
                        self.name,
                        quirk.name,
                        requirement
                    )
                })?;
            }
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

/// The limit value an inventory operation's argv renders, if any.
///
/// Recognizes both the two-token form (`--limit`, value) and the combined
/// form (`--limit=value`); `None` when the argv carries no limit at all.
fn inventory_limit_value(argv: &[String]) -> Option<String> {
    for window in argv.windows(2) {
        if window[0] == "--limit" {
            return Some(window[1].clone());
        }
    }
    argv.iter()
        .find(|argument| argument.starts_with("--limit="))
        .map(|argument| argument["--limit=".len()..].to_string())
}

/// ADR-001 axis 3: an inventory operation must render an explicit, nonzero
/// limit. The caller's ceiling arrives as the `{limit}` placeholder value; a
/// hardcoded positive literal is also explicit. Anything else — no limit at
/// all, or a zero literal — reintroduces the failures ADR-001 records: the
/// CLI default truncating priority-sorted output until low-priority beads
/// drop off the tail, and `--limit 0` answering with an empty set.
fn validate_inventory_limits(source: &Path, operation: &str, argv: &[String]) -> Result<()> {
    let Some(value) = inventory_limit_value(argv) else {
        if argv.iter().any(|argument| argument.contains("{limit}")) {
            // A backend whose limit flag is spelled differently still
            // receives the caller's explicit ceiling through the placeholder.
            return Ok(());
        }
        bail!(
            "descriptor {} operation '{}' has no explicit limit, so the backend's own \
             default would decide how much of the priority-sorted inventory is visible \
             and low-priority beads can drop off the tail; add '--limit {{limit}}' \
             (ADR-001)",
            source.display(),
            operation
        );
    };

    if value.contains("{limit}") {
        return Ok(());
    }
    match value.parse::<u64>() {
        Ok(0) => bail!(
            "descriptor {} operation '{}' hardcodes '--limit 0', which answers with an \
             empty set on backends with the limit-zero defect instead of the full \
             inventory; pass '{{limit}}' so the caller's positive limit is rendered, or \
             a positive literal (ADR-001)",
            source.display(),
            operation
        ),
        Ok(_) => Ok(()),
        Err(_) => bail!(
            "descriptor {} operation '{}' has a non-numeric '--limit {}'; pass \
             '{{limit}}' or a positive integer (ADR-001)",
            source.display(),
            operation,
            value
        ),
    }
}

fn allowed_placeholders(operation: &str) -> &'static [&'static str] {
    match operation {
        "ready" => &["limit", "assignee"],
        "list_all" => &["limit"],
        "list_in_progress" => &["limit"],
        "show" | "labels" => &["id"],
        "release" => &["id", "if_revision", "fencing_token"],
        "block" | "clear_assignee" | "reopen" => &["id", "fencing_token"],
        "clear_manual_block" => &["id"],
        "manual_blocked" => &["limit"],
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
        "close" => &["id", "reason", "fencing_token"],
        "resolve" => &[
            "id",
            "attempt_id",
            "outcome",
            "actor",
            "model",
            "harness",
            "harness_version",
            "resolve_reason",
            "evidence_ref",
            "fencing_token",
        ],
        "import" => &["input", "mode", "actor"],
        "compare" => &["id", "profile"],
        "query" => &["query"],
        "changes" => &["since"],
        "why" => &["id"],
        "ref_add" => &["id", "namespace", "key", "value"],
        "ref_remove" => &["id", "namespace", "key"],
        "ref_list" => &["id"],
        "ref_find" => &["namespace", "value"],
        "data_set" => &["id", "key", "schema_ref", "value"],
        "data_get" => &["id", "key"],
        "data_list" => &["id"],
        "data_remove" => &["id", "key"],
        "recurrence_add" => &["template", "schedule"],
        "recurrence_remove" => &["id"],
        "recurrence_list" => &[],
        "policy_validate" => &[],
        "manifest" => &["input"],
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
        ("clear_manual_block", operation(&[], None, None)),
        ("flush", operation(&[], None, None)),
        ("reopen", operation(&[], None, None)),
        ("labels", operation(&[], None, None)),
        ("label_add", operation(&[], None, None)),
        ("label_remove", operation(&[], None, None)),
        ("create", operation(&[], None, Some(ParseShape::BareId))),
        ("create_id", operation(&[], Some("bare_id"), None)),
        ("dep_add", operation(&[], None, None)),
        ("split", operation(&[], None, None)),
        (
            "manifest",
            operation(&[], None, Some(ParseShape::JsonObject)),
        ),
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
        "list_in_progress".into(),
        operation(
            &[
                "list",
                "--status",
                "in_progress",
                "--json",
                "--limit",
                "{limit}",
            ],
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
        operation(
            &[
                "release",
                "{id}",
                "--if-revision",
                "{if_revision}",
                "--fencing-token",
                "{fencing_token}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "block".into(),
        operation(
            &[
                "update",
                "{id}",
                "--status",
                "blocked",
                "--fencing-token",
                "{fencing_token}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "clear_assignee".into(),
        operation(
            &[
                "update",
                "{id}",
                "--clear-assignee",
                "--fencing-token",
                "{fencing_token}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "clear_manual_block".into(),
        operation(&["update", "{id}", "--status", "open"], None, None),
    );
    operations.insert(
        "manual_blocked".into(),
        operation(
            &[
                "list",
                "--status",
                "open",
                "--blocked",
                "--json",
                "--limit",
                "{limit}",
            ],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "flush".into(),
        operation(&["sync", "flush-only"], None, None),
    );
    operations.insert(
        "reopen".into(),
        operation(
            &["reopen", "{id}", "--fencing-token", "{fencing_token}"],
            None,
            None,
        ),
    );
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
    // bead-rs 0.2.6 exposes the versioned atomic manifest transaction.  Keep
    // this as the split strategy rather than silently routing through the
    // historical create/dep sequence.
    operations.insert(
        "split".into(),
        operation(&[], Some("transactional_batch"), None),
    );
    operations.insert(
        "manifest".into(),
        operation(
            &[
                "manifest", "commit", "--input", "{input}", "--format", "json",
            ],
            None,
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "dep_remove".into(),
        operation(&["dep", "remove", "{blocked}", "{blocker}"], None, None),
    );
    operations.insert(
        "close".into(),
        operation(
            &[
                "close",
                "{id}",
                "--reason",
                "{reason}",
                "--fencing-token",
                "{fencing_token}",
            ],
            None,
            None,
        ),
    );
    // bead-rs attempt-outcome-v1 (`bead resolve`, 0.2.6+): record one
    // attempt's outcome atomically and idempotently. NEEDLE applies the
    // lifecycle transition itself through the guarded action path, so the
    // action here is always `none`; the receipt is the durable, cross-host
    // attempt record and the input to the backend's failure-tier scheduling.
    // `--model/--harness/--harness-version` are implicit worker facts and
    // `--reason/--evidence-ref` are optional; an empty value drops the flag.
    operations.insert(
        "resolve".into(),
        operation(
            &[
                "resolve",
                "{id}",
                "--attempt-id",
                "{attempt_id}",
                "--outcome",
                "{outcome}",
                "--action",
                "none",
                "--actor",
                "{actor}",
                "--model",
                "{model}",
                "--harness",
                "{harness}",
                "--harness-version",
                "{harness_version}",
                "--reason",
                "{resolve_reason}",
                "--evidence-ref",
                "{evidence_ref}",
                "--fencing-token",
                "{fencing_token}",
                "--format",
                "json",
            ],
            None,
            Some(ParseShape::JsonObject),
        ),
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
    // bead-rs ≥ 0.2.6 `data` command: `--id`, `--namespace` (NEEDLE's `{key}`),
    // an immutable `--schema-ref`, and a JSON `--value`. The namespace must be
    // `[a-z0-9_-]+`. Verified against the installed CLI on 2026-09-12; the
    // earlier `{id} --key` spelling was never accepted by any release.
    operations.insert(
        "data_set".into(),
        operation(
            &[
                "data",
                "set",
                "--id",
                "{id}",
                "--namespace",
                "{key}",
                "--schema-ref",
                "{schema_ref}",
                "--value",
                "{value}",
            ],
            None,
            None,
        ),
    );
    operations.insert(
        "data_get".into(),
        operation(
            &[
                "data",
                "get",
                "--id",
                "{id}",
                "--namespace",
                "{key}",
                "--json",
            ],
            None,
            Some(ParseShape::JsonObject),
        ),
    );
    operations.insert(
        "data_list".into(),
        operation(
            &["data", "list", "--id", "{id}", "--json"],
            None,
            Some(ParseShape::JsonLines),
        ),
    );
    operations.insert(
        "data_remove".into(),
        operation(
            &["data", "remove", "--id", "{id}", "--namespace", "{key}"],
            None,
            None,
        ),
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
        verified_against: "bead 0.2.6 (commit 8e5839b)".to_string(),
        verified_on: "2026-09-19".to_string(),
        operations,
        capabilities: BeadBackendCapabilities {
            atomic_claim: true,
            transactional_batch: true,
            velocity_metadata: false,
        },
        quirks: vec![BeadBackendQuirk {
            name: "limit_zero_returns_empty_set".to_string(),
            // Retire by narrowing this range, never by editing the call
            // sites: once a bead-rs release returns the full inventory for
            // `--limit 0`, bump the bound below that release (or drop the
            // quirk) and the plain query — no `--limit` flag at all — takes
            // over automatically.
            version_requirement: Some("<=0.2.6".to_string()),
            description: "WHY: an unbounded list query must pass an explicit limit, because \
                `--limit 0` returns an empty set instead of the full inventory. Observed on \
                bead-forge 0.2.0 (the origin of the old unconditional `--limit 999999` \
                workaround, retired into this quirk), and on bead-rs: verified on 0.1.3 \
                (2026-08-23, needle-17fabf0a) and still present in 0.2.6 (probed 2026-09-17: \
                `list --limit 0` -> `[]`). WHICH VERSIONS: every bead-rs through 0.2.6; the \
                plain query with no `--limit` flag returns the full inventory on 0.2.6, so \
                only the explicit-zero form is broken. THE VALUE: 999999 is bead-rs's own \
                ceiling (the CLI validates `0 <= --limit <= 999999`; anything larger is a \
                usage error), and 0.1.3 accepted it too. Narrow the range once a fixed \
                release ships and re-probe with a scratch workspace before dropping."
                .to_string(),
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

        // The quirk is version-scoped, not unconditional: it covers every
        // bead-rs verified broken (`--limit 0` -> empty set) and is retired
        // by narrowing the range once a fixed release ships.
        let rs_quirk = bead_rs
            .quirks
            .iter()
            .find(|q| q.name == "limit_zero_returns_empty_set")
            .expect("bead-rs should declare limit_zero_returns_empty_set quirk");
        assert_eq!(
            rs_quirk.version_requirement.as_deref(),
            Some("<=0.2.6"),
            "bead-rs quirk must be scoped to the versions verified broken"
        );
        // The description is the retirement contract: it must say why the
        // quirk exists and for which versions.
        assert!(rs_quirk.description.contains("WHY"));
        assert!(rs_quirk.description.contains("0.1.3"));
        assert!(rs_quirk.description.contains("0.2.6"));
    }

    #[test]
    fn backend_version_parses_from_version_output() {
        assert_eq!(
            BackendVersion::parse("bead 0.2.6 (unknown 2026-09-06T14:10:37Z)"),
            Some(BackendVersion {
                major: 0,
                minor: 2,
                patch: 6
            })
        );
        assert_eq!(
            BackendVersion::parse("bf 0.2.0"),
            Some(BackendVersion {
                major: 0,
                minor: 2,
                patch: 0
            })
        );
        assert_eq!(BackendVersion::parse("no version here"), None);
        assert_eq!(BackendVersion::parse("bead 0.2 (truncated)"), None);
    }

    #[test]
    fn version_requirements_compare_component_wise() {
        let quirk = |requirement: &str| BeadBackendQuirk {
            name: "probe".to_string(),
            version_requirement: Some(requirement.to_string()),
            description: "probe".to_string(),
        };
        let version = |major: u64, minor: u64, patch: u64| {
            Some(BackendVersion {
                major,
                minor,
                patch,
            })
        };

        // 0.2.6 is in scope; 0.3.0 and 1.0.0 are past it.
        assert!(quirk("<=0.2.6").applies_to(version(0, 2, 6)));
        assert!(quirk("<=0.2.6").applies_to(version(0, 1, 3)));
        assert!(!quirk("<=0.2.6").applies_to(version(0, 3, 0)));
        assert!(!quirk("<=0.2.6").applies_to(version(1, 0, 0)));
        // String comparison would misjudge 0.10.0 against <=0.2.6; numeric
        // components must not. 0.2.10 is likewise *past* 0.2.6 (ten patches
        // after the sixth), so it falls outside the range too.
        assert!(!quirk("<=0.2.6").applies_to(version(0, 10, 0)));
        assert!(!quirk("<=0.2.6").applies_to(version(0, 2, 10)));

        // Other operators.
        assert!(quirk(">=0.3.0").applies_to(version(0, 3, 0)));
        assert!(!quirk(">=0.3.0").applies_to(version(0, 2, 6)));
        assert!(quirk("<0.3.0").applies_to(version(0, 2, 6)));
        assert!(!quirk("<0.3.0").applies_to(version(0, 3, 0)));
        assert!(quirk("=0.2.6").applies_to(version(0, 2, 6)));
        assert!(!quirk("=0.2.6").applies_to(version(0, 2, 7)));
        // A bare version is exact.
        assert!(quirk("0.2.6").applies_to(version(0, 2, 6)));
        assert!(!quirk("0.2.6").applies_to(version(0, 2, 7)));

        // An unknown runtime version conservatively applies the quirk.
        assert!(quirk("<=0.2.6").applies_to(None));
        // An unconditional quirk applies to everything.
        let unconditional = BeadBackendQuirk {
            name: "probe".to_string(),
            version_requirement: None,
            description: "probe".to_string(),
        };
        assert!(unconditional.applies_to(version(0, 2, 6)));
        assert!(unconditional.applies_to(None));
    }

    #[test]
    fn parse_version_requirement_rejects_garbage() {
        assert!(parse_version_requirement("<=0.2").is_err());
        assert!(parse_version_requirement("<=x.y.z").is_err());
        assert!(parse_version_requirement("<=").is_err());
        assert!(parse_version_requirement("").is_err());
        assert!(parse_version_requirement("~0.2.6").is_err());
    }

    #[test]
    fn descriptor_validation_rejects_malformed_quirk_version_requirement() {
        let mut backend = builtin_bead_rs();
        backend.quirks[0].version_requirement = Some("~0.2.6".to_string());
        let error = backend
            .validate(Path::new("<test>"))
            .expect_err("malformed version_requirement must fail validation");
        assert!(error.to_string().contains("invalid version_requirement"));
        assert!(error.to_string().contains("limit_zero_returns_empty_set"));
    }

    #[test]
    fn descriptor_validation_rejects_quirk_without_description() {
        let mut backend = builtin_bead_rs();
        backend.quirks[0].description = "   ".to_string();
        let error = backend
            .validate(Path::new("<test>"))
            .expect_err("empty quirk description must fail validation");
        assert!(error.to_string().contains("empty description"));
    }

    #[test]
    fn builtins_validate_with_declared_quirks() {
        let bead_rs = builtin_bead_rs();
        bead_rs
            .validate(Path::new("<builtin:bead-rs>"))
            .expect("builtin bead-rs descriptor must validate");
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
            allowed_placeholders("release"),
            &["id", "if_revision", "fencing_token"][..]
        );
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
        assert_eq!(
            allowed_placeholders("close"),
            &["id", "reason", "fencing_token"][..]
        );
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

    // ─── Inventory-limit validation tests (ADR-001 axis 3) ─────────────────────

    #[test]
    fn builtin_bead_rs_carries_explicit_limits_on_every_inventory_operation() {
        let backend = builtin_bead_rs();
        backend
            .validate(Path::new("<builtin:bead-rs>"))
            .expect("builtin bead-rs descriptor must pass the inventory-limit check");

        for operation in INVENTORY_OPERATIONS {
            let spec = backend
                .operations
                .get(*operation)
                .unwrap_or_else(|| panic!("builtin bead-rs must define '{operation}'"));
            let explicit = inventory_limit_value(&spec.argv)
                .is_some_and(|value| value.contains("{limit}"))
                || spec
                    .argv
                    .iter()
                    .any(|argument| argument.contains("{limit}"));
            assert!(
                explicit,
                "builtin bead-rs '{operation}' must render the caller's explicit limit, got {:?}",
                spec.argv
            );
        }
    }

    #[test]
    fn descriptor_without_inventory_limit_is_rejected() {
        let mut backend = builtin_bead_rs();
        backend.operations.get_mut("ready").unwrap().argv =
            vec!["list".into(), "--ready".into(), "--json".into()];

        let error = backend
            .validate(Path::new("<test>"))
            .expect_err("a limit-less ready query must fail descriptor validation");
        let message = error.to_string();
        assert!(
            message.contains("no explicit limit"),
            "error must name the missing limit, got: {message}"
        );
        assert!(
            message.contains("'ready'"),
            "error must name the offending operation, got: {message}"
        );
        assert!(
            message.contains("--limit {limit}"),
            "error must state the fix, got: {message}"
        );
        assert!(
            message.contains("low-priority"),
            "error must state the consequence, got: {message}"
        );
    }

    #[test]
    fn descriptor_hardcoding_zero_inventory_limit_is_rejected() {
        let mut backend = builtin_bead_rs();
        backend.operations.get_mut("list_all").unwrap().argv =
            vec!["list".into(), "--json".into(), "--limit".into(), "0".into()];

        let error = backend
            .validate(Path::new("<test>"))
            .expect_err("a hardcoded --limit 0 must fail descriptor validation");
        let message = error.to_string();
        assert!(
            message.contains("'--limit 0'"),
            "error must quote the zero limit, got: {message}"
        );
        assert!(
            message.contains("empty set"),
            "error must state the zero-limit consequence, got: {message}"
        );
    }

    #[test]
    fn descriptor_with_non_numeric_inventory_limit_is_rejected() {
        let mut backend = builtin_bead_rs();
        // Replace the limit outright: the builtin already carries a valid
        // `{limit}` placeholder, and the validator reads the first limit it
        // finds, so appending a second malformed one would never be seen.
        backend.operations.get_mut("manual_blocked").unwrap().argv = vec![
            "list".into(),
            "--json".into(),
            "--limit".into(),
            "all".into(),
        ];

        let error = backend
            .validate(Path::new("<test>"))
            .expect_err("a non-numeric limit must fail descriptor validation");
        assert!(
            error.to_string().contains("non-numeric"),
            "error must name the non-numeric limit, got: {}",
            error
        );
    }

    #[test]
    fn descriptor_with_positive_hardcoded_inventory_limit_is_accepted() {
        let mut backend = builtin_bead_rs();
        backend.operations.get_mut("list_in_progress").unwrap().argv = vec![
            "list".into(),
            "--status".into(),
            "in_progress".into(),
            "--limit".into(),
            "500000".into(),
        ];

        backend
            .validate(Path::new("<test>"))
            .expect("an explicit positive literal limit is not a default or zero limit");
    }

    #[test]
    fn descriptor_limit_through_placeholder_on_another_flag_is_accepted() {
        let mut backend = builtin_bead_rs();
        backend.operations.get_mut("ready").unwrap().argv = vec![
            "list".into(),
            "--ready".into(),
            "--json".into(),
            "--max".into(),
            "{limit}".into(),
        ];

        backend
            .validate(Path::new("<test>"))
            .expect("a differently spelled limit flag still receives the caller's ceiling");
    }

    #[test]
    fn combined_form_limit_literals_are_validated_too() {
        let zero = |argument: &str| {
            let mut backend = builtin_bead_rs();
            backend.operations.get_mut("ready").unwrap().argv =
                vec!["list".into(), "--ready".into(), argument.into()];
            backend.validate(Path::new("<test>")).err()
        };
        assert!(
            zero("--limit=0")
                .expect("combined-form zero limit must be rejected")
                .to_string()
                .contains("'--limit 0'"),
            "the combined form must hit the same zero-limit rejection"
        );
        assert!(zero("--limit=999999").is_none());
    }

    #[test]
    fn non_inventory_operations_do_not_require_limits() {
        let mut backend = builtin_bead_rs();
        backend.operations.get_mut("show").unwrap().argv =
            vec!["show".into(), "{id}".into(), "--json".into()];

        backend
            .validate(Path::new("<test>"))
            .expect("single-bead lookups have no inventory to truncate");
    }
}
