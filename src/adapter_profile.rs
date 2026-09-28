//! Structured profile metadata for managed agent adapters.
//!
//! An adapter's entire invocation is a shell template
//! ([`crate::dispatch::AgentAdapter::invoke_template`]), so everything
//! semantically interesting — model, reasoning effort, context mode, turn
//! ceiling — is string-encoded inside that template: invisible to validation
//! and to the attempt ledger. A *managed* adapter may declare those facts as
//! data in a `profile:` block:
//!
//! ```yaml
//! profile:
//!   version: "1"
//!   effort: max
//!   context_mode: standard
//!   max_turns: 100
//!   min_cli_version: "2.1.283"
//! ```
//!
//! The block is optional and outside `AgentAdapter`'s serde surface (unknown
//! YAML fields are ignored), so existing adapters — including every legacy
//! file in the fleet's adapters directory — load unchanged. When the block is
//! present, [`validate_yaml`] checks the declared facts against the template
//! itself and rejects contradictions (a declared effort the template never
//! passes, a 1M context mode without the `[1m]` model suffix, a trailing
//! `| cat` that masks the agent's exit status, a literal that looks like a
//! credential). [`crate::dispatch::load_adapters`] runs that validation at
//! load time, and the worker records the declared facts on the attempt
//! provenance so every `attempt.resolved` ledger row carries them.
//!
//! The rendered invocation is still the authority: the profile block
//! describes the template, and a template that drifts from its description
//! fails to load instead of silently diverging.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::dispatch::AgentAdapter;

/// Effort levels `claude --effort` accepts (verified against 2.1.283).
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Context modes a profile may declare.
///
/// `standard` is the provider default window; `1m` opts into the
/// provider-supported 1M-context identifier (`glm-5.3-flash[1m]`) plus
/// `--autocompact 1m`, and must stay a separately named canary — never a
/// silent fleet-wide default.
pub const CONTEXT_MODES: [&str; 2] = ["standard", "1m"];

/// The auth placeholder the ZAI proxy adapters use. The proxy at the tailnet
/// endpoint performs the real authentication; this literal is a documented
/// non-credential. Any other literal in `ANTHROPIC_AUTH_TOKEN` is rejected so
/// a real token can never ride an adapter file into a commit.
pub const PROXY_AUTH_PLACEHOLDER: &str = "proxy-handles-auth";

/// Structured facts a managed adapter declares about its own invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileMeta {
    /// Profile version. Bump when the invocation's semantics change so ledger
    /// rows can distinguish profile revisions.
    pub version: String,
    /// Reasoning effort the template passes (`--effort`).
    pub effort: String,
    /// Context window mode: `standard` or `1m`.
    pub context_mode: String,
    /// Agent turn ceiling the template passes (`--max-turns`).
    pub max_turns: u64,
    /// Minimum `agent_cli` version the exercised flags require. Optional:
    /// only adapters whose flags have a verified floor declare one.
    #[serde(default)]
    pub min_cli_version: Option<String>,
}

impl AgentProfileMeta {
    /// One-line summary for logs and `needle test-agent` output.
    pub fn summary(&self) -> String {
        match &self.min_cli_version {
            Some(min) => format!(
                "v{} effort={} context={} max-turns={} min-cli={}",
                self.version, self.effort, self.context_mode, self.max_turns, min
            ),
            None => format!(
                "v{} effort={} context={} max-turns={}",
                self.version, self.effort, self.context_mode, self.max_turns
            ),
        }
    }
}

/// Extract the `profile:` block from adapter YAML text, if present.
///
/// `None` means the adapter is unmanaged (no block) — not an error.
pub fn profile_from_yaml(text: &str) -> Result<Option<AgentProfileMeta>> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).context("adapter YAML did not parse")?;
    match value.get("profile") {
        None => Ok(None),
        Some(block) => {
            if block.is_null() {
                return Ok(None);
            }
            let profile: AgentProfileMeta = serde_yaml::from_value(block.clone()).context(
                "profile block is missing required fields \
                         (version, effort, context_mode, max_turns)",
            )?;
            Ok(Some(profile))
        }
    }
}

/// Parse the leading numeric version out of a CLI version string.
///
/// `claude --version` prints e.g. `2.1.283 (Claude Code)`; the first
/// `N[.N[.N]]` sequence is the version. Missing components default to 0.
pub fn parse_cli_version(text: &str) -> Option<(u64, u64, u64)> {
    fn push_component(current: &mut String, parts: &mut Vec<u64>) {
        if !current.is_empty() {
            // A run of digits longer than 20 is not a version component.
            if current.len() <= 20 {
                if let Ok(n) = current.parse::<u64>() {
                    parts.push(n);
                }
            }
            current.clear();
        }
    }
    let mut current = String::new();
    let mut parts: Vec<u64> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if ch == '.' {
            push_component(&mut current, &mut parts);
            if parts.len() == 3 {
                break;
            }
        } else {
            push_component(&mut current, &mut parts);
            if !parts.is_empty() {
                break;
            }
        }
    }
    push_component(&mut current, &mut parts);
    if parts.is_empty() {
        return None;
    }
    Some((
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    ))
}

/// Whether an installed CLI version satisfies a declared minimum.
///
/// # Errors
///
/// Returns an error when either side has no parseable version, so callers can
/// report "cannot compare" instead of guessing.
pub fn version_satisfies(installed: &str, minimum: &str) -> Result<bool> {
    let Some(installed_version) = parse_cli_version(installed) else {
        bail!("installed version has no parseable numeric version: {installed:?}");
    };
    let Some(minimum_version) = parse_cli_version(minimum) else {
        bail!("declared minimum has no parseable numeric version: {minimum:?}");
    };
    Ok(installed_version >= minimum_version)
}

/// Strip one layer of shell quoting from a template token.
fn unquote(token: &str) -> &str {
    token.trim_matches(|c| c == '"' || c == '\'')
}

/// Extract the value of every `NAME=value` assignment token named `name`,
/// with shell quoting stripped.
fn env_values(template: &str, name: &str) -> Vec<String> {
    template
        .split_whitespace()
        .filter_map(|token| token.split_once('='))
        .filter(|(var, _)| *var == name)
        .map(|(_, value)| unquote(value).to_string())
        .collect()
}

/// Extract the argument that follows every occurrence of `flag`.
fn flag_arguments(template: &str, flag: &str) -> Vec<String> {
    let tokens: Vec<&str> = template.split_whitespace().collect();
    let mut args = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if *token == flag {
            if let Some(value) = tokens.get(index + 1) {
                args.push(unquote(value).to_string());
            }
        }
    }
    args
}

/// Count occurrences of `needle` as a whole whitespace-delimited token.
fn token_count(template: &str, needle: &str) -> usize {
    template
        .split_whitespace()
        .filter(|token| *token == needle)
        .count()
}

/// Validate a managed adapter's declared profile against its template.
///
/// Every rule compares a declared fact against the template that will
/// actually run, so the two can never drift apart silently:
///
/// - `effort` is a known level and the template passes exactly that
///   `--effort` (one occurrence);
/// - `max_turns` matches the template's single `--max-turns`;
/// - `context_mode` `1m` requires the provider 1M identifier (`[1m]` suffix
///   on the requested model) *and* `--autocompact 1m`; `standard` forbids
///   both — 1M is a canary, never a hidden default;
/// - the requested model agrees with itself everywhere it appears:
///   `model:`, `--model`, and every `ANTHROPIC_*_MODEL` literal;
/// - the template keeps the streamed output contract (`stream-json` +
///   `--include-partial-messages`) when it declares the claude transform,
///   because idle-deadline liveness and tool execution need partial events;
/// - the template does not mask the agent's exit status (`| cat`);
/// - `ANTHROPIC_AUTH_TOKEN` is externalized (`${...}`) or the documented
///   proxy placeholder — never a literal that could be a credential;
/// - `min_cli_version`, when declared, parses as a numeric version.
///
/// # Errors
///
/// Returns an error naming the contradiction on the first failed rule.
pub fn validate(adapter: &AgentAdapter, profile: &AgentProfileMeta) -> Result<()> {
    let template = adapter.invoke_template.as_str();

    if profile.version.trim().is_empty() {
        bail!(
            "adapter '{}' declares a profile with an empty version",
            adapter.name
        );
    }

    if !EFFORT_LEVELS.contains(&profile.effort.as_str()) {
        bail!(
            "adapter '{}' declares unknown effort {:?} (expected one of {})",
            adapter.name,
            profile.effort,
            EFFORT_LEVELS.join(", ")
        );
    }
    let effort_flags = flag_arguments(template, "--effort");
    if effort_flags.len() != 1 || effort_flags[0] != profile.effort {
        bail!(
            "adapter '{}' declares effort {:?} but the template passes {:?}; \
             effort must be explicit and match",
            adapter.name,
            profile.effort,
            effort_flags
        );
    }

    if token_count(template, "--max-turns") != 1 {
        bail!(
            "adapter '{}' must pass --max-turns exactly once",
            adapter.name
        );
    }
    let turn_flags = flag_arguments(template, "--max-turns");
    if turn_flags.len() != 1 || turn_flags[0] != profile.max_turns.to_string() {
        bail!(
            "adapter '{}' declares max_turns {} but the template passes {:?}",
            adapter.name,
            profile.max_turns,
            turn_flags
        );
    }
    if profile.max_turns == 0 {
        bail!("adapter '{}' declares max_turns 0", adapter.name);
    }

    if !CONTEXT_MODES.contains(&profile.context_mode.as_str()) {
        bail!(
            "adapter '{}' declares unknown context_mode {:?} (expected one of {})",
            adapter.name,
            profile.context_mode,
            CONTEXT_MODES.join(", ")
        );
    }

    // The requested model must agree with itself everywhere it is stated.
    let mut requested: Vec<String> = Vec::new();
    if let Some(model) = adapter.model.as_deref() {
        requested.push(model.to_string());
    }
    requested.extend(flag_arguments(template, "--model"));
    for variable in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        requested.extend(env_values(template, variable));
    }
    if requested.iter().all(|model| model.is_empty()) {
        bail!(
            "adapter '{}' declares a profile but no requested model: set \
             `model:` and pass `--model` in the template",
            adapter.name
        );
    }
    let contradictory = requested
        .iter()
        .filter(|model| !model.is_empty())
        .any(|model| *model != *requested[0]);
    if contradictory {
        bail!(
            "adapter '{}' states contradictory requested models: {:?}",
            adapter.name,
            requested
        );
    }
    let model = requested[0].as_str();

    match profile.context_mode.as_str() {
        "1m" => {
            if !model.ends_with("[1m]") {
                bail!(
                    "adapter '{}' declares context_mode 1m but requests model \
                     {model:?} without the provider 1M identifier suffix",
                    adapter.name
                );
            }
            let autocompact = flag_arguments(template, "--autocompact");
            if autocompact.len() != 1 || autocompact[0] != "1m" {
                bail!(
                    "adapter '{}' declares context_mode 1m but the template \
                     does not pass --autocompact 1m",
                    adapter.name
                );
            }
        }
        "standard" => {
            if model.contains("[1m]") || template.contains("[1m]") {
                bail!(
                    "adapter '{}' declares context_mode standard but the \
                     template references the 1M identifier; 1M belongs on a \
                     separately named canary profile",
                    adapter.name
                );
            }
            if token_count(template, "--autocompact") != 0 {
                bail!(
                    "adapter '{}' declares context_mode standard but passes \
                     --autocompact",
                    adapter.name
                );
            }
        }
        other => bail!(
            "adapter '{}' declares unknown context_mode {other:?}",
            adapter.name
        ),
    }

    // Z.AI rejects Anthropic-compatible requests that disable thinking for
    // GLM-5.3-Flash. `--effort max` is the explicit Claude Code translation
    // used by these profiles; reject a contradictory disabled-thinking knob
    // if a future template adds one through the CLI or environment.
    let tokens: Vec<&str> = template.split_whitespace().collect();
    let thinking_disabled = tokens
        .windows(2)
        .any(|pair| pair[0] == "--thinking" && pair[1].eq_ignore_ascii_case("disabled"))
        || tokens.iter().any(|token| {
            token.eq_ignore_ascii_case("--thinking=disabled")
                || token.eq_ignore_ascii_case("ANTHROPIC_THINKING=disabled")
                || token.eq_ignore_ascii_case("CLAUDE_CODE_DISABLE_THINKING=1")
        });
    if thinking_disabled {
        bail!(
            "adapter '{}' requests disabled thinking; GLM-5.3-Flash requires \
             thinking to remain enabled/retained",
            adapter.name
        );
    }

    if template.contains("| cat") || template.contains("|cat") {
        bail!(
            "adapter '{}' masks the agent exit status with a trailing \
             `| cat`; the dispatcher runs the template under `bash -c` \
             without pipefail, so the pipeline would report cat's exit 0",
            adapter.name
        );
    }

    if adapter.output_transform.as_deref() == Some("needle-transform-claude") {
        if token_count(template, "--output-format") != 1
            || flag_arguments(template, "--output-format")
                .first()
                .map(String::as_str)
                != Some("stream-json")
        {
            bail!(
                "adapter '{}' uses needle-transform-claude but does not pass \
                 --output-format stream-json",
                adapter.name
            );
        }
        if token_count(template, "--include-partial-messages") != 1 {
            bail!(
                "adapter '{}' uses needle-transform-claude but drops \
                 --include-partial-messages; idle-deadline liveness and tool \
                 execution need the streamed partial events",
                adapter.name
            );
        }
    }

    for token in template.split_whitespace() {
        if let Some((variable, value)) = token.split_once('=') {
            if variable == "ANTHROPIC_AUTH_TOKEN" {
                let value = unquote(value);
                let externalized = value.starts_with("${");
                if !externalized && value != PROXY_AUTH_PLACEHOLDER {
                    bail!(
                        "adapter '{}' sets ANTHROPIC_AUTH_TOKEN to a literal; \
                         externalize it as ${{VAR:-placeholder}} — credentials \
                         never ride an adapter file",
                        adapter.name
                    );
                }
            }
        }
    }

    if let Some(minimum) = profile.min_cli_version.as_deref() {
        if parse_cli_version(minimum).is_none() {
            bail!(
                "adapter '{}' declares min_cli_version {minimum:?} with no \
                 parseable numeric version",
                adapter.name
            );
        }
    }

    Ok(())
}

/// Extract and validate the `profile:` block of one adapter YAML text.
///
/// `Ok(None)` — the adapter is unmanaged and passes through untouched.
///
/// # Errors
///
/// Returns an error when the block is present but malformed, or when any
/// declared fact contradicts the adapter's template.
pub fn validate_yaml(text: &str, adapter: &AgentAdapter) -> Result<Option<AgentProfileMeta>> {
    let Some(profile) = profile_from_yaml(text)? else {
        return Ok(None);
    };
    validate(adapter, &profile)
        .with_context(|| format!("adapter '{}' has an invalid profile block", adapter.name))?;
    Ok(Some(profile))
}

/// Find the managed profile of a loaded adapter by re-reading its YAML file.
///
/// Adapters load by the `name:` field inside the YAML, not the filename, so
/// the scan parses each file's name field. The first match wins; `Ok(None)`
/// means the adapter is unmanaged.
///
/// # Errors
///
/// Returns an error only when the adapters directory cannot be read or a
/// candidate file cannot be parsed — never for a missing profile.
pub fn find_profile(dir: &Path, adapter_name: &str) -> Result<Option<AgentProfileMeta>> {
    if !dir.exists() || !dir.is_dir() {
        return Ok(None);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read adapters dir: {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read adapter file: {}", path.display()))?;
        let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let matches = value
            .get("name")
            .and_then(serde_yaml::Value::as_str)
            .is_some_and(|name| name == adapter_name);
        if !matches {
            continue;
        }
        return profile_from_yaml(&text)
            .with_context(|| format!("invalid profile block in {}", path.display()));
    }
    Ok(None)
}
