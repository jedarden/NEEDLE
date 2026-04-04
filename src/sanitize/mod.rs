//! Trace sanitization: gitleaks-based secret redaction.
//!
//! Sanitizes trace content before writing to disk by applying vendored gitleaks
//! rules and workspace-specific custom patterns. No unsanitized window on disk:
//! sanitization is synchronous and always runs before any file write.
//!
//! ## Pipeline
//!
//! 1. **Keyword pre-filter** — Aho-Corasick scan skips rules with no matching
//!    keyword in the line (fast path, avoids expensive regex).
//! 2. **Regex match** — captures the secret candidate (using `secretGroup` to
//!    identify the capture group, defaulting to group 1 when present).
//! 3. **Entropy check** — Shannon entropy must meet rule threshold; low-entropy
//!    strings (placeholders, words) are not redacted.
//! 4. **Allowlist check** — global and per-rule allowlists suppress known false
//!    positives; stopwords (≈1480 in vendored config) provide word-level bypass.
//! 5. **Redact** — replaces matched secret with `[REDACTED:<rule-id>]`.
//!
//! ## Known-safe passthrough
//!
//! Certain structured fields are never redacted regardless of entropy or regex:
//! - Bead IDs (`needle-*`)
//! - The token `[REDACTED:...]` itself (already sanitized output)
//!
//! ## Custom patterns
//!
//! Workspace-specific rules live in `.needle.yaml` under
//! `learning.trace_sanitization.custom_patterns`.

use std::collections::HashSet;

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Vendored gitleaks config — embedded at compile time.
///
/// Update with `needle update-rules`.
const GITLEAKS_TOML: &str = include_str!("../../config/gitleaks.toml");

/// Default URL for `needle update-rules`.
pub const GITLEAKS_UPSTREAM_URL: &str =
    "https://raw.githubusercontent.com/gitleaks/gitleaks/main/config/gitleaks.toml";

// Minimum interesting line length: lines shorter than this can't contain a secret.
const MIN_LINE_LEN: usize = 8;

// ──────────────────────────────────────────────────────────────────────────────
// Gitleaks TOML schema
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GitleaksToml {
    #[serde(default)]
    allowlist: Option<GlobalAllowlist>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

#[derive(Debug, Deserialize, Default)]
struct GlobalAllowlist {
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    stopwords: Vec<String>,
    // `paths` is only relevant for file scanning (not text sanitization).
}

#[derive(Debug, Deserialize)]
struct RuleSpec {
    id: String,
    /// Regex pattern for content matching. Rules without `regex` (e.g. path-only
    /// rules like `pkcs12-file`) are skipped — they are irrelevant for text sanitization.
    #[serde(default)]
    regex: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    /// Which capture group holds the secret (0 = whole match, 1 = first group).
    #[serde(rename = "secretGroup", default)]
    secret_group: usize,
    /// Minimum Shannon entropy required for the secret substring.
    #[serde(default)]
    entropy: Option<f64>,
    #[serde(default)]
    allowlists: Vec<RuleAllowlist>,
}

#[derive(Debug, Deserialize)]
struct RuleAllowlist {
    #[serde(rename = "regexTarget", default)]
    regex_target: RegexTarget,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    stopwords: Vec<String>,
}

/// Which part of the match an allowlist regex is checked against.
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum RegexTarget {
    /// Check against the captured secret substring.
    #[default]
    Secret,
    /// Check against the entire regex match.
    Match,
    /// Check against the whole input line.
    Line,
}

// ──────────────────────────────────────────────────────────────────────────────
// Compiled rule
// ──────────────────────────────────────────────────────────────────────────────

struct CompiledRule {
    id: String,
    regex: Regex,
    /// Aho-Corasick automaton for keyword pre-filter (lowercased keywords).
    /// `None` when the rule has no keywords.
    keywords: Option<AhoCorasick>,
    /// Capture group index for the secret value (0 = full match).
    secret_group: usize,
    entropy_threshold: Option<f64>,
    allowlist_regexes: Vec<(Regex, RegexTarget)>,
    allowlist_stopwords: HashSet<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

/// A workspace-specific custom sanitization pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPattern {
    /// Rule identifier used in `[REDACTED:<id>]` output.
    pub id: String,
    /// Regex pattern. Capture group 1 is the secret; if absent, whole match is used.
    pub pattern: String,
    /// Optional minimum Shannon entropy threshold.
    #[serde(default)]
    pub entropy: Option<f64>,
}

/// Sanitizes text content by redacting secrets.
///
/// Build once and reuse across traces — rule compilation is expensive.
pub struct Sanitizer {
    rules: Vec<CompiledRule>,
    global_stopwords: HashSet<String>,
    global_allowlist_regexes: Vec<Regex>,
    safe_passthrough: AhoCorasick,
}

impl Sanitizer {
    /// Build a sanitizer from the vendored gitleaks config and optional custom patterns.
    pub fn new(custom_patterns: &[CustomPattern]) -> Result<Self> {
        Self::from_toml(GITLEAKS_TOML, custom_patterns)
    }

    /// Build a sanitizer from an arbitrary gitleaks TOML string.
    ///
    /// Used by `needle update-rules` to validate a downloaded config before
    /// writing it to disk.
    pub fn from_toml(toml_str: &str, custom_patterns: &[CustomPattern]) -> Result<Self> {
        let config: GitleaksToml =
            toml::from_str(toml_str).context("failed to parse gitleaks TOML")?;

        let global = config.allowlist.unwrap_or_default();
        let global_stopwords: HashSet<String> = global
            .stopwords
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();
        let global_allowlist_regexes: Vec<Regex> = global
            .regexes
            .iter()
            .filter_map(|r| {
                let normalized = normalize_regex(r);
                Regex::new(&normalized)
                    .map_err(|e| {
                        tracing::debug!(
                            rule = "global-allowlist",
                            error = %e,
                            pattern = %r,
                            "skipping invalid allowlist regex"
                        );
                    })
                    .ok()
            })
            .collect();

        let mut rules: Vec<CompiledRule> =
            Vec::with_capacity(config.rules.len() + custom_patterns.len());

        for spec in &config.rules {
            match compile_rule(spec) {
                Some(r) => rules.push(r),
                None => tracing::debug!(
                    rule_id = %spec.id,
                    "skipping gitleaks rule: regex failed to compile"
                ),
            }
        }

        for custom in custom_patterns {
            let normalized = normalize_regex(&custom.pattern);
            match Regex::new(&normalized) {
                Ok(regex) => {
                    // Custom patterns: use capture group 1 when present, else group 0.
                    let secret_group = if regex.captures_len() > 1 { 1 } else { 0 };
                    rules.push(CompiledRule {
                        id: custom.id.clone(),
                        regex,
                        keywords: None,
                        secret_group,
                        entropy_threshold: custom.entropy,
                        allowlist_regexes: Vec::new(),
                        allowlist_stopwords: HashSet::new(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pattern_id = %custom.id,
                        error = %e,
                        "custom sanitization pattern failed to compile, skipping"
                    );
                }
            }
        }

        // Known-safe passthrough: these substrings in the *secret* portion mean
        // we skip redaction unconditionally.
        let safe_passthrough = AhoCorasick::new(["needle-", "[REDACTED:"])
            .context("failed to build safe-passthrough automaton")?;

        Ok(Sanitizer {
            rules,
            global_stopwords,
            global_allowlist_regexes,
            safe_passthrough,
        })
    }

    /// Sanitize a string, replacing matched secrets with `[REDACTED:<rule-id>]`.
    ///
    /// Runs synchronously — must complete before any write to disk.
    pub fn sanitize(&self, text: &str) -> String {
        if text.len() < MIN_LINE_LEN {
            return text.to_string();
        }

        let trailing_newline = text.ends_with('\n');
        let lines: Vec<&str> = text.lines().collect();
        let sanitized = lines
            .iter()
            .map(|line| self.sanitize_line(line))
            .collect::<Vec<_>>()
            .join("\n");

        if trailing_newline {
            sanitized + "\n"
        } else {
            sanitized
        }
    }

    fn sanitize_line(&self, line: &str) -> String {
        if line.len() < MIN_LINE_LEN {
            return line.to_string();
        }

        let mut result = line.to_string();
        for rule in &self.rules {
            result = self.apply_rule(rule, &result, line);
        }
        result
    }

    fn apply_rule(&self, rule: &CompiledRule, text: &str, original_line: &str) -> String {
        // Keyword pre-filter: if the rule declares keywords and none appear in
        // the lowercased text, skip the regex entirely.
        if let Some(ref ac) = rule.keywords {
            let lower = text.to_ascii_lowercase();
            if !ac.is_match(lower.as_bytes()) {
                return text.to_string();
            }
        }

        let mut result = text.to_string();
        let mut scan_start = 0usize;

        loop {
            // Search in the remaining tail of `result`.
            let haystack = &result[scan_start..];
            let caps = match rule.regex.captures(haystack) {
                Some(c) => c,
                None => break,
            };

            let full_match = caps.get(0).unwrap();

            // Determine which group holds the secret.
            let secret_match = if rule.secret_group > 0 && rule.secret_group < caps.len() {
                caps.get(rule.secret_group)
            } else if caps.len() > 1 {
                // Default: use group 1 when available.
                caps.get(1)
            } else {
                caps.get(0)
            };

            let secret_m = match secret_match {
                Some(m) => m,
                None => {
                    scan_start += full_match.end();
                    continue;
                }
            };

            let secret_str = secret_m.as_str();

            // Entropy gate: skip low-entropy strings (placeholders, env var names).
            if let Some(threshold) = rule.entropy_threshold {
                if shannon_entropy(secret_str) < threshold {
                    scan_start += full_match.end();
                    continue;
                }
            }

            // Global stopwords (lowercased comparison).
            let secret_lower = secret_str.to_lowercase();
            if self.global_stopwords.contains(&secret_lower) {
                scan_start += full_match.end();
                continue;
            }

            // Global allowlist regexes checked against the secret.
            if self
                .global_allowlist_regexes
                .iter()
                .any(|r| r.is_match(secret_str))
            {
                scan_start += full_match.end();
                continue;
            }

            // Known-safe passthrough: bead IDs, already-redacted strings.
            if self.safe_passthrough.is_match(secret_str.as_bytes()) {
                scan_start += full_match.end();
                continue;
            }

            // Per-rule allowlist checks.
            let mut allowed = false;
            for (al_re, target) in &rule.allowlist_regexes {
                let subject = match target {
                    RegexTarget::Secret => secret_str,
                    RegexTarget::Match => full_match.as_str(),
                    RegexTarget::Line => original_line,
                };
                if al_re.is_match(subject) {
                    allowed = true;
                    break;
                }
            }
            if allowed {
                scan_start += full_match.end();
                continue;
            }

            // Per-rule stopwords.
            if rule.allowlist_stopwords.contains(&secret_lower) {
                scan_start += full_match.end();
                continue;
            }

            // Redact: replace only the secret group span with [REDACTED:<id>].
            let redaction = format!("[REDACTED:{}]", rule.id);

            let abs_secret_start = scan_start + secret_m.start();
            let abs_secret_end = scan_start + secret_m.end();
            let abs_full_end = scan_start + full_match.end();

            result = format!(
                "{}{}{}",
                &result[..abs_secret_start],
                redaction,
                &result[abs_secret_end..]
            );

            // Advance past the (now possibly length-changed) full match.
            let delta = redaction.len() as isize - (abs_secret_end - abs_secret_start) as isize;
            scan_start = ((abs_full_end as isize) + delta) as usize;
        }

        result
    }

    /// Number of successfully compiled rules (gitleaks + custom).
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Returns the embedded vendored gitleaks TOML text.
    pub fn vendored_toml() -> &'static str {
        GITLEAKS_TOML
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn compile_rule(spec: &RuleSpec) -> Option<CompiledRule> {
    // Path-only rules (e.g. pkcs12-file) have no regex — skip them silently.
    let raw_regex = spec.regex.as_deref()?;
    let normalized = normalize_regex(raw_regex);
    let regex = Regex::new(&normalized)
        .map_err(|e| {
            tracing::debug!(
                rule_id = %spec.id,
                error = %e,
                pattern = %raw_regex,
                "gitleaks rule regex compile error"
            );
        })
        .ok()?;

    let keywords = if spec.keywords.is_empty() {
        None
    } else {
        let lower_kw: Vec<String> = spec.keywords.iter().map(|k| k.to_lowercase()).collect();
        AhoCorasick::new(&lower_kw)
            .map_err(|e| {
                tracing::debug!(
                    rule_id = %spec.id,
                    error = %e,
                    "failed to build keyword automaton"
                );
            })
            .ok()
    };

    let mut allowlist_regexes: Vec<(Regex, RegexTarget)> = Vec::new();
    let mut allowlist_stopwords: HashSet<String> = HashSet::new();

    for al in &spec.allowlists {
        for re_str in &al.regexes {
            let normalized = normalize_regex(re_str);
            match Regex::new(&normalized) {
                Ok(re) => allowlist_regexes.push((re, al.regex_target)),
                Err(e) => tracing::debug!(
                    rule_id = %spec.id,
                    error = %e,
                    pattern = %re_str,
                    "skipping invalid allowlist regex"
                ),
            }
        }
        for sw in &al.stopwords {
            allowlist_stopwords.insert(sw.to_lowercase());
        }
    }

    Some(CompiledRule {
        id: spec.id.clone(),
        regex,
        keywords,
        secret_group: spec.secret_group,
        entropy_threshold: spec.entropy,
        allowlist_regexes,
        allowlist_stopwords,
    })
}

/// Normalize a gitleaks regex for compatibility with the Rust `regex` crate.
///
/// Converts POSIX character class syntax (`[[:alnum:]]` etc.) to equivalent
/// Rust regex syntax, which is RE2-based and does not support POSIX classes.
fn normalize_regex(pattern: &str) -> String {
    pattern
        .replace("[[:alnum:]]", "[a-zA-Z0-9]")
        .replace("[[:alpha:]]", "[a-zA-Z]")
        .replace("[[:digit:]]", "[0-9]")
        .replace("[[:lower:]]", "[a-z]")
        .replace("[[:upper:]]", "[A-Z]")
        .replace("[[:space:]]", r"[\t\n\r ]")
        .replace("[[:print:]]", r"[\x20-\x7e]")
        .replace("[[:ascii:]]", r"[\x00-\x7f]")
}

/// Calculate Shannon entropy of a string (base-2, over byte values).
///
/// Used to distinguish high-entropy secrets from low-entropy placeholders.
fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for byte in s.bytes() {
        counts[byte as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sanitizer() -> Sanitizer {
        Sanitizer::new(&[]).expect("failed to build sanitizer from vendored config")
    }

    #[test]
    fn sanitizer_builds_from_vendored_toml() {
        let s = make_sanitizer();
        // Vendored config has 222 rules; allow for minor variation if updated.
        assert!(
            s.rule_count() >= 200,
            "expected >= 200 compiled rules, got {}",
            s.rule_count()
        );
    }

    #[test]
    fn sanitizer_redacts_anthropic_api_key() {
        let s = make_sanitizer();
        // Anthropic API key format: sk-ant-api03-<93 chars>AA
        let fake_key = format!(
            "sk-ant-api03-{:0>93}AA",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-ABCDEFGHIJKLMNOPQRSTU"
        );
        let line = format!("ANTHROPIC_API_KEY={}", fake_key);
        let result = s.sanitize(&line);
        assert!(
            result.contains("[REDACTED:anthropic-api-key]"),
            "expected redaction, got: {}",
            result
        );
        assert!(!result.contains(&fake_key), "key should be redacted");
    }

    #[test]
    fn sanitizer_redacts_gcp_api_key() {
        let s = make_sanitizer();
        // GCP API key: AIza + 35 alphanumeric/dash chars (high entropy)
        let fake_key = "AIzaSyBnFb9RkQ3mD2eWl8TpXa0vN7hJcK4oMiY";
        let line = format!("key = \"{}\"", fake_key);
        let result = s.sanitize(&line);
        assert!(
            result.contains("[REDACTED:gcp-api-key]"),
            "expected gcp-api-key redaction, got: {}",
            result
        );
    }

    #[test]
    fn sanitizer_preserves_bead_ids() {
        let s = make_sanitizer();
        // Bead IDs must never be redacted.
        let line = "processing bead needle-wysd.2.2 in workspace";
        let result = s.sanitize(line);
        assert_eq!(result, line, "bead ID should not be redacted");
    }

    #[test]
    fn sanitizer_preserves_already_redacted() {
        let s = make_sanitizer();
        let line = "token=[REDACTED:anthropic-api-key] was sanitized";
        let result = s.sanitize(line);
        assert_eq!(result, line, "already-redacted token should pass through");
    }

    #[test]
    fn sanitizer_handles_low_entropy_placeholder() {
        let s = make_sanitizer();
        // A placeholder like "AAAAAAAAAAAAAAAAAAA" has zero entropy and should
        // not be redacted even if it looks like an API key.
        let line = "AIzaAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let result = s.sanitize(line);
        // Low entropy — should not be redacted.
        assert!(
            !result.contains("[REDACTED:"),
            "low-entropy placeholder should not be redacted"
        );
    }

    #[test]
    fn sanitizer_with_custom_pattern() {
        let custom = vec![CustomPattern {
            id: "test-key".to_string(),
            pattern: r"(mykey-[a-f0-9]{32})".to_string(),
            entropy: None,
        }];
        let s = Sanitizer::new(&custom).unwrap();
        let fake = "mykey-deadbeefcafedeadbeefcafedeadbeef";
        let line = format!("key={}", fake);
        let result = s.sanitize(&line);
        assert!(
            result.contains("[REDACTED:test-key]"),
            "custom pattern should redact, got: {}",
            result
        );
    }

    #[test]
    fn sanitizer_multiline_sanitizes_each_line() {
        let s = make_sanitizer();
        let fake_key = format!(
            "sk-ant-api03-{:0>93}AA",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-ABCDEFGHIJKLMNOPQRSTU"
        );
        let text = format!(
            "line1=innocent\nANTHROPIC_KEY={}\nline3=also-innocent\n",
            fake_key
        );
        let result = s.sanitize(&text);
        assert!(result.contains("line1=innocent"));
        assert!(result.contains("[REDACTED:anthropic-api-key]"));
        assert!(result.contains("line3=also-innocent"));
        assert!(result.ends_with('\n'), "trailing newline preserved");
    }

    #[test]
    fn shannon_entropy_empty() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn shannon_entropy_uniform() {
        // All same chars → entropy 0.
        assert_eq!(shannon_entropy("aaaaaaa"), 0.0);
    }

    #[test]
    fn shannon_entropy_high() {
        // Random-looking base64 → high entropy.
        let e = shannon_entropy("aB3xK9mPqRnW5vYzTdHcEjFuGsOlIi7");
        assert!(e > 3.5, "expected high entropy, got {}", e);
    }

    #[test]
    fn normalize_regex_posix_classes() {
        assert_eq!(normalize_regex("[[:alnum:]]"), "[a-zA-Z0-9]");
        assert_eq!(normalize_regex("[[:digit:]]"), "[0-9]");
    }

    #[test]
    fn sanitizer_preserves_short_line() {
        let s = make_sanitizer();
        let short = "abc123";
        assert_eq!(s.sanitize(short), short);
    }

    #[test]
    fn sanitizer_performance() {
        // Sanitization of ~60KB trace should complete quickly.
        // Release: < 10ms (acceptance criterion).
        // Debug: < 500ms (unoptimized build headroom).
        let s = make_sanitizer();
        let line = "INFO: processing request with token=someValue and other data ".repeat(100);
        let text = line.repeat(10); // ~60KB
        let start = std::time::Instant::now();
        let _ = s.sanitize(&text);
        let elapsed_ms = start.elapsed().as_millis();
        #[cfg(debug_assertions)]
        let threshold_ms = 500u128;
        #[cfg(not(debug_assertions))]
        let threshold_ms = 10u128;
        assert!(
            elapsed_ms < threshold_ms,
            "sanitization took {}ms, expected < {}ms",
            elapsed_ms,
            threshold_ms
        );
    }
}
