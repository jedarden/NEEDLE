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

use std::borrow::Cow;
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

/// Statistics from the keyword pre-filter skip rate measurement.
#[derive(Debug, Clone)]
pub struct SkipStats {
    /// Total number of rule checks performed (lines × rules).
    pub total_checks: usize,
    /// Number of rule checks skipped by the Aho-Corasick keyword pre-filter.
    pub skipped_by_keywords: usize,
    /// Fraction of checks skipped (0.0 to 1.0).
    pub skip_rate: f64,
}

impl SkipStats {
    /// Format statistics for display.
    pub fn format(&self) -> String {
        format!(
            "{}/{} skipped ({:.1}%)",
            self.skipped_by_keywords,
            self.total_checks,
            self.skip_rate * 100.0
        )
    }
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
                let normalized = normalize_gitleaks_regex(r);
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
        let mut lower = result.to_ascii_lowercase();
        for rule in &self.rules {
            if let Some(ref ac) = rule.keywords {
                if !ac.is_match(lower.as_bytes()) {
                    continue;
                }
            }
            if let Cow::Owned(redacted) = self.apply_rule(rule, &result, line) {
                lower = redacted.to_ascii_lowercase();
                result = redacted;
            }
        }
        result
    }

    fn apply_rule<'a>(
        &self,
        rule: &CompiledRule,
        text: &'a str,
        original_line: &str,
    ) -> Cow<'a, str> {
        // Most rules do not match. Borrow the input until a redaction actually
        // changes it, rather than copying every line for every compiled rule.
        let mut result = Cow::Borrowed(text);
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

            result = Cow::Owned(format!(
                "{}{}{}",
                &result[..abs_secret_start],
                redaction,
                &result[abs_secret_end..]
            ));

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

    /// Measure keyword pre-filter skip statistics for given text.
    ///
    /// Returns statistics about how many rules were skipped by the
    /// Aho-Corasick keyword pre-filter (i.e., rules whose keywords
    /// were not found in the text, allowing them to be skipped
    /// without running the expensive regex).
    pub fn measure_skip_stats(&self, text: &str) -> SkipStats {
        let mut total_checks = 0usize;
        let mut skipped_by_keywords = 0usize;

        for line in text.lines() {
            if line.len() < MIN_LINE_LEN {
                continue;
            }

            let lower = line.to_ascii_lowercase();
            for rule in &self.rules {
                total_checks += 1;
                if let Some(ref ac) = rule.keywords {
                    if !ac.is_match(lower.as_bytes()) {
                        skipped_by_keywords += 1;
                    }
                }
            }
        }

        SkipStats {
            total_checks,
            skipped_by_keywords,
            skip_rate: if total_checks > 0 {
                skipped_by_keywords as f64 / total_checks as f64
            } else {
                0.0
            },
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn compile_rule(spec: &RuleSpec) -> Option<CompiledRule> {
    // Path-only rules (e.g. pkcs12-file) have no regex — skip them silently.
    let raw_regex = spec.regex.as_deref()?;
    let normalized = normalize_gitleaks_regex(raw_regex);
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
            let normalized = normalize_gitleaks_regex(re_str);
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

/// Preserve Go/RE2's ASCII Perl classes when importing Gitleaks patterns.
///
/// Rust's default `\w` expands to Unicode word characters. Repeating it in
/// hundreds of rules made the vendored sanitizer allocate about 440 MiB per
/// worker and caused three rules to exceed the regex compilation size limit.
/// Explicit ASCII classes retain Gitleaks semantics and reduce that to about
/// 16 MiB. Keep Unicode mode enabled for dots, negated classes, properties, and
/// case folding; disabling it for the whole regex changes those semantics.
///
/// Consume escapes together so a literal `\\w` is not rewritten. Nested
/// character classes are supported by Rust regex, so these expansions also
/// work inside a class such as `[\w.-]`. Workspace custom patterns use Rust
/// syntax and deliberately do not pass through this conversion.
fn normalize_gitleaks_regex(pattern: &str) -> String {
    let mut result = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }
        match chars.next() {
            Some('w') => result.push_str("[[:word:]]"),
            Some('W') => result.push_str("[[:^word:]]"),
            Some('d') => result.push_str("[0-9]"),
            Some('D') => result.push_str("[^0-9]"),
            Some('s') => result.push_str(r"[\t\n\f\r ]"),
            Some('S') => result.push_str(r"[^\t\n\f\r ]"),
            Some('b') => result.push_str(r"(?-u:\b)"),
            Some('B') => result.push_str(r"(?-u:\B)"),
            Some(escaped) => {
                result.push('\\');
                result.push(escaped);
            }
            None => result.push('\\'),
        }
    }
    result
}

/// Normalize legacy POSIX class spellings in workspace custom patterns.
///
/// Keep custom-pattern behavior separate from the Go/RE2 import conversion:
/// their Perl classes and word boundaries remain Unicode-aware Rust regexes.
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
        let config: GitleaksToml = toml::from_str(GITLEAKS_TOML).unwrap();
        let expected = config.rules.iter().filter(|r| r.regex.is_some()).count();
        assert_eq!(s.rule_count(), expected, "every content rule must compile");
    }

    #[test]
    fn gitleaks_perl_classes_follow_go_semantics() {
        // Go's Perl classes are ASCII-only, even inside another class.
        for (pattern, matching, nonmatching) in [
            (r"^\w+$", "abc_123", "café"),
            (r"^[\w.-]+$", "api.key-123", "clé"),
            (r"^\W+$", "é!", "A"),
            (r"^\d+$", "123", "١٢٣"),
            (r"^\D+$", "١٢٣", "123"),
            (r"^\s+$", "\t\n\u{c}\r ", "\u{b}\u{a0}"),
            (r"^\S+$", "\u{b}\u{a0}", "\t "),
            (r"\btoken\b", "étokené", "xtokenx"),
            (r"\Btoken\B", "xtokenx", "étokené"),
            // Dots, negated classes, Unicode properties, and case folding
            // still operate on Unicode characters, as they do in Go.
            (r"^.$", "é", "ab"),
            (r"^[^x]$", "é", "x"),
            (r"^\p{Greek}+$", "αβ", "ab"),
            (r"(?i)^k$", "K", "x"),
        ] {
            let re = Regex::new(&normalize_gitleaks_regex(pattern)).unwrap();
            assert!(re.is_match(matching), "pattern {pattern}");
            assert!(!re.is_match(nonmatching), "pattern {pattern}");
        }
        // POSIX whitespace includes vertical tab; Perl whitespace does not.
        let re = Regex::new(&normalize_gitleaks_regex(r"^[[:space:]]+$")).unwrap();
        assert!(re.is_match("\u{b}\u{c}"));
    }

    #[test]
    fn gitleaks_normalization_preserves_literal_escapes_and_capture_groups() {
        let re = Regex::new(&normalize_gitleaks_regex(r"^(\\w):(\w+):(\\d)$")).unwrap();
        let caps = re.captures(r"\w:abc_123:\d").unwrap();
        assert_eq!(&caps[1], r"\w");
        assert_eq!(&caps[2], "abc_123");
        assert_eq!(&caps[3], r"\d");
    }

    #[test]
    fn custom_patterns_keep_unicode_semantics() {
        let custom = [CustomPattern {
            id: "unicode-test".into(),
            pattern: r"credential=(\w+)".into(),
            entropy: None,
        }];
        let s = Sanitizer::from_toml("", &custom).unwrap();
        assert_eq!(
            s.sanitize("credential=秘密é١"),
            "credential=[REDACTED:unicode-test]"
        );
    }

    #[test]
    fn gitleaks_allowlists_use_ascii_classes() {
        let s = Sanitizer::from_toml(
            r#"
                [allowlist]
                regexes = ['^\w+$']
                [[rules]]
                id = 'allowlist-test'
                regex = 'credential=(\S+)'
            "#,
            &[],
        )
        .unwrap();
        assert_eq!(s.sanitize("credential=abc_123"), "credential=abc_123");
        assert_eq!(
            s.sanitize("credential=秘密١"),
            "credential=[REDACTED:allowlist-test]"
        );
        let s = Sanitizer::from_toml(
            r#"
                [[rules]]
                id = 'allowlist-test'
                regex = 'credential=(\S+)'
                [[rules.allowlists]]
                regexes = ['^\d+$']
            "#,
            &[],
        )
        .unwrap();
        assert_eq!(s.sanitize("credential=123"), "credential=123");
        assert_eq!(
            s.sanitize("credential=١٢٣"),
            "credential=[REDACTED:allowlist-test]"
        );
    }

    #[test]
    fn keyword_filter_tracks_redactions_from_previous_rules() {
        let s = Sanitizer::from_toml(
            r#"
                [[rules]]
                id = 'markerword'
                regex = 'first=(\w+)'
                keywords = ['first']
                [[rules]]
                id = 'second-rule'
                regex = 'second=(\w+)'
                keywords = ['markerword']
            "#,
            &[],
        )
        .unwrap();
        assert_eq!(
            s.sanitize("first=example second=another"),
            "first=[REDACTED:markerword] second=[REDACTED:second-rule]"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sanitizer_memory_budget() {
        // Run alone in a child test process so parallel tests and allocator
        // reuse cannot hide the startup peak. This never starts a worker.
        const PROBE: &str = "NEEDLE_SANITIZER_MEMORY_PROBE";
        if std::env::var_os(PROBE).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sanitize::tests::sanitizer_memory_budget",
                    "--nocapture",
                ])
                .env(PROBE, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "memory probe failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let peak_kib = || {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("VmHWM:"))
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<usize>()
                .unwrap()
        };
        let before = peak_kib();
        let s = make_sanitizer();
        let after = peak_kib();
        assert!(s.rule_count() >= 200);
        assert!(
            after.saturating_sub(before) < 64 * 1024,
            "sanitizer startup grew peak RSS by {} KiB; budget is 64 MiB",
            after.saturating_sub(before)
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
        let fake_key = [
            "AI", "za", "SyBn", "Fb9R", "kQ3m", "D2eW", "l8Tp", "Xa0v", "N7hJ", "cK4o", "MiY",
        ]
        .concat();
        let line = format!("key = \"{}\"", fake_key);
        let result = s.sanitize(&line);
        assert!(
            result.contains("[REDACTED:gcp-api-key]"),
            "expected gcp-api-key redaction, got: {}",
            result
        );
    }

    #[test]
    fn sanitizer_preserves_explicit_api_digest() {
        let s = make_sanitizer();
        let digest = "0123456789abcdef".repeat(4);
        let line = format!("API digest: {digest}");
        assert_eq!(s.sanitize(&line), line);
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

        // Track skip rate metrics
        let skip_stats = s.measure_skip_stats(&text);

        let start = std::time::Instant::now();
        let _ = s.sanitize(&text);
        let elapsed_ms = start.elapsed().as_millis();

        #[cfg(debug_assertions)]
        let threshold_ms = 500u128;
        #[cfg(not(debug_assertions))]
        let threshold_ms = 10u128;

        // Report metrics together
        eprintln!(
            "Performance test - Latency: {}ms, Skip rate: {:.1}%",
            elapsed_ms,
            skip_stats.skip_rate * 100.0
        );

        assert!(
            elapsed_ms < threshold_ms,
            "sanitization took {}ms, expected < {}ms",
            elapsed_ms,
            threshold_ms
        );

        // Verify skip rate is being calculated
        assert!(
            skip_stats.total_checks > 0,
            "Should have performed some rule checks"
        );
        assert!(
            skip_stats.skip_rate >= 0.0 && skip_stats.skip_rate <= 1.0,
            "Skip rate should be between 0 and 1, got {}",
            skip_stats.skip_rate
        );
    }

    #[test]
    fn sanitizer_performance_100kb_median() {
        // Phase 4 success criterion: sanitization of 100KB trace must
        // complete in <10ms median on release builds (<500ms on debug).
        // This test measures median latency over multiple samples for
        // more stable results than a single measurement.

        let s = make_sanitizer();

        // Generate representative 100KB trace content (JSONL format).
        let events = [
            r#"{"type":"system","subtype":"init","cwd":"/home/coding/NEEDLE","session_id":"abc123","tools":["Task","Read","Write"],"model":"claude-sonnet-4-6"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Processing bead needle-test.123"}}}"#,
            r#"{"type":"tool_use","name":"Bash","input":{"command":"echo 'test'}}}"#,
            r#"{"type":"tool_result","output":"test output","exit_code":0}"#,
            r#"{"type":"system","subtype":"bead_update","bead_id":"needle-wysd.2.2","status":"completed"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Token=[REDACTED:anthropic-api-key] sanitized"}}}"#,
        ];

        let avg_event_size = events.iter().map(|s| s.len() + 1).sum::<usize>() / events.len();
        let events_needed = (100 * 1024) / avg_event_size;

        let mut trace = String::with_capacity(100 * 1024);
        for i in 0..events_needed {
            trace.push_str(events[i % events.len()]);
            trace.push('\n');
        }
        trace.truncate(100 * 1024);

        // Measure skip rate before latency measurement.
        let skip_stats = s.measure_skip_stats(&trace);
        eprintln!("Skip rate: {}", skip_stats.format());

        // Measure latency over multiple samples to get stable median.
        const SAMPLE_COUNT: usize = 20;
        let mut latencies = Vec::with_capacity(SAMPLE_COUNT);

        // Warm-up.
        for _ in 0..3 {
            let _ = s.sanitize(&trace);
        }

        for _ in 0..SAMPLE_COUNT {
            let start = std::time::Instant::now();
            let _ = s.sanitize(&trace);
            latencies.push(start.elapsed().as_millis());
        }

        latencies.sort();
        let median = latencies[SAMPLE_COUNT / 2];
        // Use proper linear interpolation p95 calculation for accuracy.
        let p95 = crate::stats::calculate_p95(&latencies);

        #[cfg(debug_assertions)]
        let threshold_ms = 2000u128;
        #[cfg(not(debug_assertions))]
        let threshold_ms = 10u128;

        // Use environment variable to override threshold (for CI tuning).
        let threshold_ms = std::env::var("SANITIZER_LATENCY_THRESHOLD_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(threshold_ms);

        // Report all metrics together.
        eprintln!(
            "Metrics - Median: {} ms, p95: {} ms, Skip rate: {:.1}%",
            median,
            p95,
            skip_stats.skip_rate * 100.0
        );

        assert!(
            median < threshold_ms,
            "Sanitizer median latency ({} ms, p95: {} ms) exceeds threshold ({} ms) over {} samples",
            median, p95, threshold_ms, SAMPLE_COUNT
        );

        // Verify skip rate is being tracked and is reasonable.
        // A reasonable skip rate should be > 0% (pre-filter is working)
        // and < 100% (some rules are matching keywords).
        assert!(
            skip_stats.skip_rate > 0.0 && skip_stats.skip_rate < 1.0,
            "Skip rate should be between 0% and 100%, got {:.1}%",
            skip_stats.skip_rate * 100.0
        );
    }

    // ── Skip rate tracking tests ─────────────────────────────────────────────────────────

    #[test]
    fn skip_stats_calculates_correct_rate() {
        let s = make_sanitizer();

        // Test content with predictable keyword patterns
        let text = "API_KEY=sk-test-token\nDATABASE_URL=postgresql://localhost:5432/test\n";

        let stats = s.measure_skip_stats(text);

        // Should have performed some checks
        assert!(stats.total_checks > 0, "Should perform rule checks");

        // Skip rate should be between 0 and 1
        assert!(
            stats.skip_rate >= 0.0 && stats.skip_rate <= 1.0,
            "Skip rate should be between 0 and 1, got {}",
            stats.skip_rate
        );

        // The calculation formula: (skipped / total) * 100
        let expected_rate = if stats.total_checks > 0 {
            stats.skipped_by_keywords as f64 / stats.total_checks as f64
        } else {
            0.0
        };
        assert!(
            (stats.skip_rate - expected_rate).abs() < f64::EPSILON,
            "Skip rate calculation incorrect: expected {}, got {}",
            expected_rate,
            stats.skip_rate
        );
    }

    #[test]
    fn skip_stats_format_displays_correctly() {
        let stats = SkipStats {
            total_checks: 1000,
            skipped_by_keywords: 850,
            skip_rate: 0.85,
        };

        let formatted = stats.format();
        assert_eq!(formatted, "850/1000 skipped (85.0%)");
    }

    #[test]
    fn skip_rate_tracked_across_trace_sizes() {
        let s = make_sanitizer();

        // Test different trace sizes to ensure skip rate tracking works consistently
        let sizes = vec![
            ("10KB", 10 * 1024),
            ("50KB", 50 * 1024),
            ("100KB", 100 * 1024),
        ];

        let mut metrics = Vec::new();

        for (label, size) in sizes {
            let text = "x".repeat(size);
            let stats = s.measure_skip_stats(&text);

            metrics.push((label, size, stats.clone()));

            // Verify metrics are collected
            assert!(
                stats.total_checks > 0,
                "Should perform checks for {} trace",
                label
            );

            // Verify skip rate is valid
            assert!(
                stats.skip_rate >= 0.0 && stats.skip_rate <= 1.0,
                "Skip rate should be valid for {} trace, got {}",
                label,
                stats.skip_rate
            );

            eprintln!(
                "{}: {} - Skip rate: {:.1}%",
                label,
                stats.format(),
                stats.skip_rate * 100.0
            );
        }

        // All metrics should be stored successfully
        assert_eq!(
            metrics.len(),
            3,
            "Should collect metrics for all trace sizes"
        );
    }

    #[test]
    fn skip_rate_stored_with_latency_metrics() {
        let s = make_sanitizer();
        let text = "Processing bead needle-test.123 with API_KEY=sk-placeholder\n".repeat(1000);

        // Measure skip rate
        let skip_stats = s.measure_skip_stats(&text);

        // Measure latency
        let start = std::time::Instant::now();
        let _ = s.sanitize(&text);
        let latency_ms = start.elapsed().as_millis();

        // Create metrics bundle (simulating storage with other metrics)
        let metrics = format!(
            "Latency: {}ms, Skip rate: {:.1}%, Checks: {}, Skipped: {}",
            latency_ms,
            skip_stats.skip_rate * 100.0,
            skip_stats.total_checks,
            skip_stats.skipped_by_keywords
        );

        // Verify metrics string contains both latency and skip rate
        assert!(metrics.contains("Latency:"), "Should include latency");
        assert!(metrics.contains("Skip rate:"), "Should include skip rate");
        assert!(metrics.contains("Checks:"), "Should include total checks");
        assert!(metrics.contains("Skipped:"), "Should include skipped count");

        eprintln!("Combined metrics: {}", metrics);

        // Verify values are reasonable
        assert!(skip_stats.total_checks > 0, "Should perform rule checks");
        assert!(latency_ms < 1_000, "Latency should remain below one second");
    }
}
