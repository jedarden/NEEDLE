//! Routing rule matcher for model-to-adapter dispatch.
//!
//! Provides pattern matching logic for routing rules: regex/glob patterns
//! against model names, first-match-wins semantics, and default fallback.

use crate::config::RoutingRule;
use std::sync::Arc;

/// Compiled routing rule with cached glob pattern for efficient matching.
#[derive(Debug, Clone)]
struct CompiledRule {
    /// Compiled glob pattern matcher.
    glob_matcher: Option<Arc<glob::Pattern>>,
    /// Raw glob source, needed at match time to special-case a bare `*`/`**`
    /// (see `matches()`).
    glob_source: Option<String>,
    /// Fallback regex matcher for patterns that aren't valid globs.
    regex_matcher: Option<Arc<regex::Regex>>,
    /// Adapter to use on match.
    adapter: String,
}

impl CompiledRule {
    /// Compile a routing rule into an efficient matcher.
    ///
    /// Supports both regex patterns (any valid regex) and glob-style patterns:
    /// - `*` matches any sequence of non-separator characters
    /// - `**` matches any sequence of characters, including slashes
    /// - `?` matches any single character
    /// - `[a-z]` matches any character in the bracket
    /// - `[!a-z]` matches any character not in the bracket
    ///
    /// Detects whether a pattern should be treated as regex or glob based on
    /// the presence of regex metacharacters. If the pattern contains regex
    /// features (like `.*`, `^`, `$`, etc.), it's compiled as regex. Otherwise,
    /// if it contains glob wildcards (`*`, `?`, etc.), it's compiled as glob
    /// using the glob crate for efficient pattern matching.
    fn from_rule(rule: &RoutingRule) -> Result<Self, String> {
        let pattern = &rule.match_model;
        let adapter = rule.adapter.clone();

        // First, check if the pattern should be treated as regex
        // If it has clear regex features, compile as regex
        if !needs_glob_conversion(pattern) {
            // Try regex compilation
            match regex::Regex::new(pattern) {
                Ok(regex) => {
                    return Ok(CompiledRule {
                        glob_matcher: None,
                        glob_source: None,
                        regex_matcher: Some(Arc::new(regex)),
                        adapter,
                    })
                }
                Err(_) => {
                    // Regex failed, try glob as fallback
                }
            }
        }

        // Try glob pattern compilation using the glob crate
        match glob::Pattern::new(pattern) {
            Ok(glob_pattern) => {
                Ok(CompiledRule {
                    glob_matcher: Some(Arc::new(glob_pattern)),
                    glob_source: Some(pattern.clone()),
                    regex_matcher: None,
                    adapter,
                })
            }
            Err(e) => {
                // Glob compilation failed, return error
                Err(format!("Failed to compile glob pattern '{}': {}", pattern, e))
            }
        }
    }

    /// Test if a model name matches this rule.
    fn matches(&self, model: &str) -> bool {
        // Prefer glob matching if available. require_literal_separator ensures
        // a single `*` doesn't cross `/` (only `**` should span path segments) —
        // the glob crate's default matches() allows `*` to cross separators,
        // which would let e.g. "provider/*" wrongly match "provider/foo/model".
        //
        // Exception: a *bare* "*" or "**" (the whole pattern, no literal
        // segments) is a full catch-all by convention — it's meant to match
        // any model name at all, slashes included, the same way a default
        // fallback rule would.
        if let Some(ref glob) = self.glob_matcher {
            let is_bare_wildcard = matches!(self.glob_source.as_deref(), Some("*") | Some("**"));
            return glob.matches_with(
                model,
                glob::MatchOptions {
                    case_sensitive: true,
                    require_literal_separator: !is_bare_wildcard,
                    require_literal_leading_dot: false,
                },
            );
        }

        // Fall back to regex matching
        if let Some(ref regex) = self.regex_matcher {
            return regex.is_match(model);
        }

        false
    }
}

/// Check if a pattern is a glob pattern.
///
/// A simple heuristic that returns true if the pattern contains glob-style
/// wildcards (`*` or `**`). This is a basic check that doesn't attempt to
/// distinguish between glob and regex patterns - it just reports whether
/// the pattern contains wildcard characters.
///
/// # Arguments
///
/// * `pattern` - The pattern string to check.
///
/// # Returns
///
/// * `true` if the pattern contains `*` or `**`.
/// * `false` otherwise.
///
/// # Examples
///
/// ```
/// use needle::routing::is_glob_pattern;
///
/// // Glob patterns with wildcards
/// assert!(is_glob_pattern("*"));
/// assert!(is_glob_pattern("**"));
/// assert!(is_glob_pattern("gpt-*"));
/// assert!(is_glob_pattern("test/**"));
///
/// // Non-glob patterns (no wildcards)
/// assert!(!is_glob_pattern("exact-match"));
/// assert!(!is_glob_pattern("gpt-4"));
/// assert!(!is_glob_pattern("claude-sonnet"));
/// ```
pub fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*')
}

/// Check if a pattern contains glob-style wildcards that need conversion.
///
/// Returns true if the pattern appears to be a glob pattern rather than
/// a well-formed regex. The heuristic checks for:
/// 1. Regex features: anchors (^$), character classes [], parentheses (),
///    quantifiers +?{}, alternation |, or the regex wildcard sequence .*
/// 2. If present, it's a regex pattern - no conversion needed
/// 3. If absent, but * is present, it's a glob pattern - convert it
fn needs_glob_conversion(pattern: &str) -> bool {
    // First, check if it looks like a regex by looking for regex metacharacters.
    // The key indicator is .* which is "any character" in regex but not in glob.
    let has_regex_features = pattern.contains(|c| {
        matches!(
            c,
            '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '?' | '|' | '\\'
        )
    }) || pattern.contains(".*");

    if has_regex_features {
        // Has clear regex features - treat as regex
        return false;
    }

    // At this point, we have a simple pattern that might be glob or regex.
    // If it contains unescaped * (and we already ruled out other regex chars),
    // treat it as a glob pattern.
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            // Skip escaped character.
            i += 2;
            continue;
        }
        if chars[i] == '*' {
            return true;
        }
        i += 1;
    }

    false
}

/// Match a model name against routing rules, returning the adapter to use.
///
/// Evaluates rules in order; the first matching rule determines the adapter.
/// If no rule matches, returns the default adapter.
///
/// # Arguments
///
/// * `model` - The model name to match (e.g., "claude-sonnet-4-6").
/// * `rules` - Ordered list of routing rules (first match wins).
/// * `default` - Fallback adapter when no rules match.
///
/// # Returns
///
/// * `Some(adapter)` - The adapter name to use.
/// * `None` - No rule matched and default is empty/invalid (caller should handle).
///
/// # Examples
///
/// ```
/// use needle::routing::match_adapter;
/// use needle::config::RoutingRule;
///
/// let rules = vec![
///     RoutingRule {
///         match_model: "sonnet.*".to_string(),
///         adapter: "claude-print".to_string(),
///     },
///     RoutingRule {
///         match_model: "*".to_string(),
///         adapter: "default-adapter".to_string(),
///     },
/// ];
///
/// // First rule matches.
/// assert_eq!(
///     match_adapter("sonnet-4-6", &rules, "fallback"),
///     Some("claude-print".to_string())
/// );
///
/// // Second rule (catch-all) matches.
/// assert_eq!(
///     match_adapter("other-model", &rules, "fallback"),
///     Some("default-adapter".to_string())
/// );
/// ```
pub fn match_adapter(model: &str, rules: &[RoutingRule], default: &str) -> Option<String> {
    // Compile rules and test in order (first match wins).
    for rule in rules {
        match CompiledRule::from_rule(rule) {
            Ok(compiled) => {
                if compiled.matches(model) {
                    return Some(compiled.adapter.clone());
                }
            }
            Err(e) => {
                // Log the error but continue with other rules.
                // Invalid patterns are skipped rather than failing the entire dispatch.
                tracing::warn!(
                    pattern = %rule.match_model,
                    error = %e,
                    "invalid routing pattern — skipping rule"
                );
            }
        }
    }

    // No rule matched — use default if provided.
    if default.is_empty() {
        None
    } else {
        Some(default.to_string())
    }
}

/// Match a model name against a single glob pattern using the glob crate.
///
/// This is a lower-level function that tests if a glob pattern matches a model name.
/// It uses the `glob` crate's Pattern type directly for glob-style pattern matching.
///
/// # Arguments
///
/// * `pattern` - Glob pattern to match (e.g., "claude-*", "gpt-*", "*").
/// * `model_name` - The model name to test against the pattern.
///
/// # Returns
///
/// * `Some(())` - The pattern matches the model name.
/// * `None` - The pattern does not match or is invalid.
///
/// # Edge Cases
///
/// * Empty pattern: Returns `None` (no match).
/// * Empty model name: Returns `None` (no match).
/// * Invalid glob pattern: Returns `None` (no match).
///
/// # Glob Pattern Syntax
///
/// * `*` - Matches any sequence of non-separator characters.
/// * `**` - Matches any sequence of characters, including path separators.
/// * `?` - Matches any single non-separator character.
/// * `[a-z]` - Matches any character in the bracket.
/// * `[!a-z]` - Matches any character not in the bracket.
///
/// # Examples
///
/// ```
/// use needle::routing::match_adapter_with_glob;
///
/// // Match with wildcard
/// assert!(match_adapter_with_glob("claude-*", "claude-sonnet-4-6").is_some());
/// assert!(match_adapter_with_glob("claude-*", "claude-opus-4-6").is_some());
/// assert!(match_adapter_with_glob("claude-*", "gpt-4").is_none());
///
/// // Match with double wildcard
/// assert!(match_adapter_with_glob("**", "any-model").is_some());
///
/// // Edge cases
/// assert!(match_adapter_with_glob("", "model").is_none());
/// assert!(match_adapter_with_glob("pattern", "").is_none());
/// ```
pub fn match_adapter_with_glob(pattern: &str, model_name: &str) -> Option<()> {
    // Handle edge cases
    if pattern.is_empty() || model_name.is_empty() {
        return None;
    }

    // Use the glob crate to compile and match the pattern
    match glob::Pattern::new(pattern) {
        Ok(glob_pattern) => {
            // The glob crate's Pattern::matches takes a &str directly
            if glob_pattern.matches(model_name) {
                Some(())
            } else {
                None
            }
        }
        Err(_) => {
            // Invalid glob pattern
            None
        }
    }
}

/// Match a model name against a glob pattern.
///
/// This function uses the `glob` crate's Pattern functionality to perform
/// glob-style pattern matching against model names.
///
/// # Arguments
///
/// * `pattern` - Glob pattern to match (e.g., "claude-*", "gpt-*", "*").
/// * `model_name` - The model name to test against the pattern.
///
/// # Returns
///
/// * `true` - The pattern matches the model name.
/// * `false` - The pattern does not match, pattern is invalid, or either argument is empty.
///
/// # Glob Pattern Syntax
///
/// * `*` - Matches any sequence of non-separator characters.
/// * `**` - Matches any sequence of characters, including path separators.
/// * `?` - Matches any single non-separator character.
/// * `[a-z]` - Matches any character in the bracket.
/// * `[!a-z]` - Matches any character not in the bracket.
///
/// # Examples
///
/// ```
/// use needle::routing::match_glob;
///
/// // Match with wildcard
/// assert!(match_glob("claude-*", "claude-sonnet-4-6"));
/// assert!(match_glob("claude-*", "claude-opus-4-6"));
/// assert!(!match_glob("claude-*", "gpt-4"));
///
/// // Match with double wildcard
/// assert!(match_glob("**", "any-model"));
///
/// // Edge cases
/// assert!(!match_glob("", "model"));  // Empty pattern
/// assert!(!match_glob("pattern", ""));  // Empty model name
/// ```
pub fn match_glob(pattern: &str, model_name: &str) -> bool {
    // Handle edge cases
    if pattern.is_empty() || model_name.is_empty() {
        return false;
    }

    // Use the glob crate to compile and match the pattern
    match glob::Pattern::new(pattern) {
        Ok(glob_pattern) => glob_pattern.matches(model_name),
        Err(_) => false, // Invalid glob pattern
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoutingRule;

    fn make_rule(pattern: &str, adapter: &str) -> RoutingRule {
        RoutingRule {
            match_model: pattern.to_string(),
            adapter: adapter.to_string(),
        }
    }

    #[test]
    fn regex_pattern_match() {
        let rules = vec![make_rule("sonnet.*", "claude-print")];
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("sonnet-4-5-new", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
    }

    #[test]
    fn regex_pattern_complex() {
        let rules = vec![make_rule("(claude-)?(sonnet|opus).*", "claude-print")];
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("opus-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-opus-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        // Does not match.
        assert_eq!(
            match_adapter("haiku-4-5", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn glob_asterisk_single() {
        // * matches any non-slash characters.
        let rules = vec![make_rule("claude-*", "claude-print")];
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-opus", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-haiku-4-5", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        // Does not match (different prefix).
        assert_eq!(
            match_adapter("anthropic-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn glob_asterisk_double() {
        // ** matches any characters including slashes. Note: the underlying
        // glob crate requires a recursive wildcard to form its own path
        // component (rejects a pattern like "claude**" glued to a literal —
        // "recursive wildcards must form a single path component"), so the
        // literal prefix must end in `/` for `**` to be valid here.
        let rules = vec![make_rule("claude/**", "claude-print")];
        assert_eq!(
            match_adapter("claude/sonnet", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude/any/nested/path", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        // No `/` after "claude" — doesn't satisfy the required literal
        // separator before the recursive wildcard, so it falls through.
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn glob_catchall() {
        // Catch-all pattern with *.
        let rules = vec![make_rule("*", "default-adapter")];
        assert_eq!(
            match_adapter("anything", &rules, "fallback"),
            Some("default-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("default-adapter".to_string())
        );
        assert_eq!(
            match_adapter("provider/model", &rules, "fallback"),
            Some("default-adapter".to_string())
        );
    }

    #[test]
    fn glob_catchall_double_asterisk() {
        // Catch-all pattern with **.
        let rules = vec![make_rule("**", "default-adapter")];
        assert_eq!(
            match_adapter("anything", &rules, "fallback"),
            Some("default-adapter".to_string())
        );
        assert_eq!(
            match_adapter("nested/path/model", &rules, "fallback"),
            Some("default-adapter".to_string())
        );
    }

    #[test]
    fn first_match_wins() {
        let rules = vec![
            make_rule("claude.*", "first-adapter"),
            make_rule("claude-sonnet.*", "second-adapter"), // Never matched.
            make_rule("*", "catchall"),
        ];

        // First rule matches.
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("first-adapter".to_string())
        );
        // Third rule matches.
        assert_eq!(
            match_adapter("other-model", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn no_match_returns_default() {
        let rules = vec![make_rule("sonnet.*", "claude-print")];
        assert_eq!(
            match_adapter("other-model", &rules, "fallback"),
            Some("fallback".to_string())
        );
        assert_eq!(
            match_adapter("opus-4-6", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn no_match_empty_rules() {
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(
            match_adapter("any-model", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn no_match_empty_default_returns_none() {
        let rules = vec![make_rule("sonnet.*", "claude-print")];
        assert_eq!(match_adapter("other-model", &rules, ""), None);
    }

    #[test]
    fn empty_rules_empty_default_returns_none() {
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(match_adapter("any-model", &rules, ""), None);
    }

    #[test]
    fn invalid_regex_pattern_skipped_gracefully() {
        let rules = vec![
            make_rule("[invalid(regex", "bad-adapter"), // Invalid regex.
            make_rule("sonnet.*", "good-adapter"),
        ];

        // Invalid pattern is skipped, second rule matches.
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("good-adapter".to_string())
        );
    }

    #[test]
    fn all_rules_invalid_returns_default() {
        let rules = vec![
            make_rule("[invalid(regex", "bad-adapter"),
            make_rule("(unclosed", "also-bad"),
        ];

        // All patterns invalid — fall back to default.
        assert_eq!(
            match_adapter("any-model", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn escaped_asterisk_treated_literally() {
        // Escaped asterisk should be treated literally, not as a glob wildcard.
        let rules = vec![make_rule(r"model\*", "escaped-adapter")];
        assert_eq!(
            match_adapter("model*", &rules, "fallback"),
            Some("escaped-adapter".to_string())
        );
        // Should NOT match "model-xyz" (asterisk is literal).
        assert_eq!(
            match_adapter("model-xyz", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn escaped_double_asterisk_treated_literally() {
        // Escaped ** should be treated literally.
        let rules = vec![make_rule(r"model\*\*", "escaped-adapter")];
        assert_eq!(
            match_adapter("model**", &rules, "fallback"),
            Some("escaped-adapter".to_string())
        );
        // Should NOT match "model-xyz".
        assert_eq!(
            match_adapter("model-xyz", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn regex_anchors_work() {
        // Regex anchors should work correctly.
        let rules = vec![make_rule("^sonnet$", "exact-adapter")];
        assert_eq!(
            match_adapter("sonnet", &rules, "fallback"),
            Some("exact-adapter".to_string())
        );
        // Does NOT match "sonnet-4-6" (^ and $ anchor to entire string).
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn mixed_regex_and_glob_patterns() {
        // Mix of regex and glob patterns in same ruleset.
        let rules = vec![
            make_rule("^claude-sonnet-.*", "sonnet-adapter"),
            // Glob style. Leading `*` is needed since "opus*"/"haiku*" are
            // prefix-anchored (glob patterns match the whole string) and
            // wouldn't match "claude-opus-..."/"claude-haiku-...".
            make_rule("*opus*", "opus-adapter"),
            make_rule("*haiku*", "haiku-adapter"),
        ];

        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("sonnet-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-opus-4-6", &rules, "fallback"),
            Some("opus-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-haiku-4-5", &rules, "fallback"),
            Some("haiku-adapter".to_string())
        );
        // No match.
        assert_eq!(
            match_adapter("other-model", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn glob_pattern_with_slashes() {
        // * should not match slashes (single path segment).
        let rules = vec![make_rule("provider/*", "adapter")];
        assert_eq!(
            match_adapter("provider/model", &rules, "fallback"),
            Some("adapter".to_string())
        );
        // Does NOT match (two slashes).
        assert_eq!(
            match_adapter("provider/foo/model", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn glob_double_asterisk_with_slashes() {
        // ** should match slashes (multi-segment).
        let rules = vec![make_rule("provider/**", "adapter")];
        assert_eq!(
            match_adapter("provider/model", &rules, "fallback"),
            Some("adapter".to_string())
        );
        assert_eq!(
            match_adapter("provider/foo/model", &rules, "fallback"),
            Some("adapter".to_string())
        );
        assert_eq!(
            match_adapter("provider/foo/bar/model", &rules, "fallback"),
            Some("adapter".to_string())
        );
    }

    #[test]
    fn is_glob_pattern_detection() {
        // Glob patterns with wildcards
        assert!(is_glob_pattern("*"));
        assert!(is_glob_pattern("**"));
        assert!(is_glob_pattern("gpt-*"));
        assert!(is_glob_pattern("test/**"));

        // Non-glob patterns (no wildcards)
        assert!(!is_glob_pattern("exact-match"));
        assert!(!is_glob_pattern("gpt-4"));
        assert!(!is_glob_pattern("claude-sonnet"));

        // Mixed patterns
        assert!(is_glob_pattern("claude-*"));
        assert!(is_glob_pattern("provider/**"));
        assert!(is_glob_pattern("*-sonnet-*"));
    }

    #[test]
    fn needs_glob_conversion_detection() {
        // Plain strings (no wildcards, no regex characters) don't need conversion.
        assert!(!needs_glob_conversion("hello"));
        assert!(!needs_glob_conversion("model-name"));
        assert!(!needs_glob_conversion("claude_sonnet"));

        // Patterns with unescaped * need conversion.
        assert!(needs_glob_conversion("pattern*"));
        assert!(needs_glob_conversion("**"));
        assert!(needs_glob_conversion("claude-*"));

        // Escaped asterisks don't need conversion.
        assert!(!needs_glob_conversion(r"pattern\*"));
        assert!(!needs_glob_conversion(r"\*\*"));

        // Regex patterns without glob characters don't need conversion.
        assert!(!needs_glob_conversion("sonnet.*"));
        assert!(!needs_glob_conversion("^claude$"));
        assert!(!needs_glob_conversion("[a-z]+"));
    }

    #[test]
    fn compiled_rule_matches() {
        let rule = CompiledRule::from_rule(&make_rule("sonnet.*", "adapter")).unwrap();
        assert!(rule.matches("sonnet-4-6"));
        assert!(rule.matches("claude-sonnet-4-6"));
        assert!(!rule.matches("opus-4-6"));
    }

    #[test]
    fn compiled_rule_invalid_pattern() {
        let result = CompiledRule::from_rule(&make_rule("[invalid", "adapter"));
        assert!(result.is_err());
    }

    #[test]
    fn real_world_anthropic_routing() {
        // Real-world example: subscription vs API billing.
        let rules = vec![
            make_rule("(claude-)?(sonnet|opus|fable|haiku).*", "claude-print"),
            make_rule("*", "claude-code-glm-4.7"),
        ];

        // Anthropic models -> subscription (claude-print).
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-opus-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-fable-5", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude-haiku-4-5-20251001", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("opus-4-6", &rules, "fallback"),
            Some("claude-print".to_string())
        );

        // Other models -> API billing (claude-code-glm-4.7).
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("claude-code-glm-4.7".to_string())
        );
        assert_eq!(
            match_adapter("gemini-pro", &rules, "fallback"),
            Some("claude-code-glm-4.7".to_string())
        );
    }

    #[test]
    fn adapter_names_preserved() {
        // Adapter names should be returned exactly as specified.
        let rules = vec![make_rule("sonnet.*", "Claude-Print-v2.0")];
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("Claude-Print-v2.0".to_string())
        );
    }

    #[test]
    fn empty_model_name() {
        let rules = vec![make_rule("*", "adapter")];
        // Empty model should match catch-all.
        assert_eq!(
            match_adapter("", &rules, "fallback"),
            Some("adapter".to_string())
        );
    }

    #[test]
    fn whitespace_in_patterns() {
        let rules = vec![make_rule("claude sonnet", "adapter")];
        // Spaces are literal characters in regex/glob.
        assert_eq!(
            match_adapter("claude sonnet", &rules, "fallback"),
            Some("adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn gpt_regex_patterns() {
        // Test GPT patterns as mentioned in acceptance criteria.
        // Using proper regex pattern with .* for wildcard matching.
        let rules = vec![make_rule("gpt-.*", "openai-adapter")];

        // Matches GPT models.
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );
        assert_eq!(
            match_adapter("gpt-3.5", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );
        assert_eq!(
            match_adapter("gpt-4-turbo", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );

        // Does not match non-GPT models.
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
        assert_eq!(
            match_adapter("gemini-pro", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn gpt_glob_style_patterns() {
        // Test glob-style GPT patterns (single asterisk).
        // Glob * converts to [^/]+ (non-slash characters).
        let rules = vec![make_rule("gpt-*", "openai-adapter")];

        // Matches GPT models.
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );
        assert_eq!(
            match_adapter("gpt-3.5", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );
        assert_eq!(
            match_adapter("gpt-4-turbo", &rules, "fallback"),
            Some("openai-adapter".to_string())
        );

        // Does not match non-GPT models.
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn claude_family_regex() {
        // Test Claude family patterns.
        let rules = vec![make_rule("claude-.*", "claude-adapter")];

        // Matches Claude models.
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-opus-4-6", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-haiku-4-5", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("claude-fable-5", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );

        // Does not match non-Claude models.
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn non_matching_regex_patterns() {
        // Test that non-matching patterns correctly return default.
        let rules = vec![make_rule("^gpt-4$", "exact-adapter")];

        // Exact match works.
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("exact-adapter".to_string())
        );

        // Non-matching models fall back to default.
        assert_eq!(
            match_adapter("gpt-3.5", &rules, "fallback"),
            Some("fallback".to_string())
        );
        assert_eq!(
            match_adapter("gpt-4-turbo", &rules, "fallback"),
            Some("fallback".to_string())
        );
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for match_adapter_with_glob
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn glob_match_with_wildcard() {
        // Single asterisk wildcard
        assert!(match_adapter_with_glob("claude-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("claude-*", "claude-opus-4-6").is_some());
        assert!(match_adapter_with_glob("claude-*", "claude-haiku-4-5").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-4").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-3.5").is_some());

        // Non-matching patterns
        assert!(match_adapter_with_glob("claude-*", "gpt-4").is_none());
        assert!(match_adapter_with_glob("gpt-*", "claude-sonnet").is_none());
    }

    #[test]
    fn glob_match_catchall() {
        // Catch-all pattern with *
        assert!(match_adapter_with_glob("*", "any-model").is_some());
        assert!(match_adapter_with_glob("*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("*", "gpt-4").is_some());
        assert!(match_adapter_with_glob("*", "provider/model").is_some());
    }

    #[test]
    fn glob_match_double_wildcard() {
        // Double asterisk matches any characters including slashes
        assert!(match_adapter_with_glob("**", "any-model").is_some());
        assert!(match_adapter_with_glob("**", "provider/model").is_some());
        assert!(match_adapter_with_glob("**", "nested/path/model").is_some());
    }

    #[test]
    fn glob_match_question_mark() {
        // Question mark matches exactly one character
        assert!(match_adapter_with_glob("gpt-?", "gpt-4").is_some());
        assert!(match_adapter_with_glob("gpt-?", "gpt-3").is_some());
        assert!(match_adapter_with_glob("gpt-?", "gpt-35").is_none()); // Two chars
        assert!(match_adapter_with_glob("gpt-?", "gpt-").is_none()); // Zero chars after dash
        assert!(match_adapter_with_glob("gpt-?", "gpt-4.5").is_none()); // Has extra chars
    }

    #[test]
    fn glob_match_character_class() {
        // Character class [a-z]
        assert!(match_adapter_with_glob("claude-[a-z]*", "claude-sonnet").is_some());
        assert!(match_adapter_with_glob("claude-[a-z]*", "claude-opus").is_some());
        assert!(match_adapter_with_glob("gpt-[0-9]", "gpt-4").is_some());
        assert!(match_adapter_with_glob("gpt-[0-9]", "gpt-35").is_none()); // Two digits
    }

    #[test]
    fn glob_match_empty_pattern() {
        // Empty pattern returns None
        assert!(match_adapter_with_glob("", "model").is_none());
        assert!(match_adapter_with_glob("", "").is_none());
    }

    #[test]
    fn glob_match_empty_model_name() {
        // Empty model name returns None
        assert!(match_adapter_with_glob("pattern", "").is_none());
        assert!(match_adapter_with_glob("", "").is_none());
        assert!(match_adapter_with_glob("*", "").is_none());
    }

    #[test]
    fn glob_match_exact_string() {
        // Exact string match without wildcards
        assert!(match_adapter_with_glob("claude-sonnet-4-6", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("gpt-4", "gpt-4").is_some());
        assert!(match_adapter_with_glob("claude-sonnet-4-6", "claude-opus-4-6").is_none());
        assert!(match_adapter_with_glob("gpt-4", "gpt-3.5").is_none());
    }

    #[test]
    fn glob_match_complex_patterns() {
        // More complex patterns
        assert!(match_adapter_with_glob("*-sonnet-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("*-sonnet-*", "anthropic-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("*-sonnet-*", "claude-opus-4-6").is_none());

        assert!(match_adapter_with_glob("claude-*-4-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("claude-*-4-*", "claude-opus-4-6").is_some());
        assert!(match_adapter_with_glob("claude-*-4-*", "claude-sonnet-3-5").is_none());
    }

    #[test]
    fn glob_match_with_slashes() {
        // Patterns with path separators
        assert!(match_adapter_with_glob("provider/*", "provider/model").is_some());
        assert!(match_adapter_with_glob("provider/*", "provider/gpt-4").is_some());
        assert!(match_adapter_with_glob("provider/*", "other/model").is_none());

        // Double wildcard with slashes
        assert!(match_adapter_with_glob("provider/**", "provider/model").is_some());
        assert!(match_adapter_with_glob("provider/**", "provider/nested/model").is_some());
    }

    #[test]
    fn glob_match_invalid_pattern() {
        // Invalid glob patterns return None
        // The glob crate is more permissive than regex, so most patterns are valid
        // But we can test extreme cases
        assert!(match_adapter_with_glob("[", "model").is_none()); // Unclosed bracket
    }

    #[test]
    fn glob_match_real_world_patterns() {
        // Real-world model routing patterns
        assert!(match_adapter_with_glob("claude-sonnet-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("claude-opus-*", "claude-opus-4-6").is_some());
        assert!(match_adapter_with_glob("claude-haiku-*", "claude-haiku-4-5").is_some());
        assert!(match_adapter_with_glob("claude-fable-*", "claude-fable-5").is_some());

        // OpenAI models
        assert!(match_adapter_with_glob("gpt-*", "gpt-4").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-3.5-turbo").is_some());

        // Generic catch-all
        assert!(match_adapter_with_glob("*", "any-unknown-model").is_some());
    }

    #[test]
    fn glob_match_case_sensitive() {
        // Glob matching is case-sensitive
        assert!(match_adapter_with_glob("Claude-*", "Claude-Sonnet").is_some());
        assert!(match_adapter_with_glob("Claude-*", "claude-sonnet").is_none());
        assert!(match_adapter_with_glob("claude-*", "CLAUDE-SONNET").is_none());
    }

    #[test]
    fn glob_match_with_special_characters() {
        // Model names with special characters
        assert!(match_adapter_with_glob("claude-*", "claude-sonnet_4_6").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-4.turbo").is_some());
        assert!(match_adapter_with_glob("*", "model-with-dashes").is_some());
        assert!(match_adapter_with_glob("*", "model_with_underscores").is_some());
    }

    #[test]
    fn glob_match_nested_path_test_pattern() {
        // Test patterns for nested paths with "test" in them
        // Using *test* to match anything containing "test"
        assert!(match_adapter_with_glob("*test*", "test").is_some());
        assert!(match_adapter_with_glob("*test*", "foo-test").is_some());
        assert!(match_adapter_with_glob("*test*", "foo-test-bar").is_some());
        assert!(match_adapter_with_glob("*test*", "testing").is_some());

        // Path-based test patterns
        assert!(match_adapter_with_glob("**/test", "test").is_some());
        assert!(match_adapter_with_glob("**/test", "foo/test").is_some());
        assert!(match_adapter_with_glob("**/test", "foo/bar/test").is_some());
        assert!(match_adapter_with_glob("**/test", "foo/bar/baz/test").is_some());

        // Non-matching patterns
        assert!(match_adapter_with_glob("**/test", "testing").is_none());
        assert!(match_adapter_with_glob("**/test", "foo/testing").is_none());
        assert!(match_adapter_with_glob("**/test", "foo/atest/bar").is_none());
        assert!(match_adapter_with_glob("**/test", "test/more").is_none()); // Not a leaf
    }

    #[test]
    fn glob_match_double_asterisk_specific_patterns() {
        // More specific double-asterisk patterns
        assert!(match_adapter_with_glob("**/model", "model").is_some());
        assert!(match_adapter_with_glob("**/model", "provider/model").is_some());
        assert!(match_adapter_with_glob("**/model", "a/b/c/model").is_some());

        assert!(match_adapter_with_glob("provider/**", "provider/model").is_some());
        assert!(match_adapter_with_glob("provider/**", "provider/nested/model").is_some());
        assert!(match_adapter_with_glob("provider/**", "provider/").is_some());

        // Non-matching
        assert!(match_adapter_with_glob("**/model", "other").is_none());
        assert!(match_adapter_with_glob("provider/**", "other/model").is_none());
    }

    #[test]
    fn glob_match_empty_string_variations() {
        // Comprehensive empty string tests
        assert!(match_adapter_with_glob("*", "").is_none()); // Empty model with wildcard
        assert!(match_adapter_with_glob("**", "").is_none()); // Empty model with double wildcard
        assert!(match_adapter_with_glob("pattern", "").is_none()); // Empty model with pattern
        assert!(match_adapter_with_glob("", "model").is_none()); // Empty pattern
        assert!(match_adapter_with_glob("", "").is_none()); // Both empty
    }

    #[test]
    fn glob_match_bracket_patterns() {
        // Test negated character classes
        assert!(match_adapter_with_glob("gpt-[!0-9]", "gpt-a").is_some());
        assert!(match_adapter_with_glob("gpt-[!0-9]", "gpt-x").is_some());
        assert!(match_adapter_with_glob("gpt-[!0-9]", "gpt-4").is_none());

        // Test ranges
        assert!(match_adapter_with_glob("model-[a-c]", "model-a").is_some());
        assert!(match_adapter_with_glob("model-[a-c]", "model-b").is_some());
        assert!(match_adapter_with_glob("model-[a-c]", "model-c").is_some());
        assert!(match_adapter_with_glob("model-[a-c]", "model-d").is_none());
    }

    #[test]
    fn glob_match_multiple_wildcards() {
        // Test multiple wildcards in same pattern
        assert!(match_adapter_with_glob("*-*", "claude-sonnet").is_some());
        assert!(match_adapter_with_glob("*-*", "gpt-4").is_some());
        assert!(match_adapter_with_glob("*-*", "model").is_none()); // No dash

        assert!(match_adapter_with_glob("*-*-*", "claude-sonnet-4").is_some());
        assert!(match_adapter_with_glob("*-*-*", "a-b-c").is_some());
        assert!(match_adapter_with_glob("*-*-*", "a-b").is_none()); // Only two parts
    }

    #[test]
    fn glob_match_trailing_wildcard() {
        // Test trailing wildcards with single asterisk
        assert!(match_adapter_with_glob("claude-*", "claude-sonnet").is_some());
        assert!(match_adapter_with_glob("claude-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("claude-*", "claude").is_none()); // Need at least one char after dash

        // Test trailing wildcards with double asterisk
        assert!(match_adapter_with_glob("claude/**", "claude/sonnet").is_some());
        assert!(match_adapter_with_glob("claude/**", "claude/sonnet/4").is_some());
        assert!(match_adapter_with_glob("claude/**", "claude").is_none()); // Need slash after claude

        // Test patterns ending with various wildcards
        assert!(match_adapter_with_glob("*-4", "gpt-4").is_some());
        assert!(match_adapter_with_glob("*-4", "claude-sonnet-4").is_some());
        assert!(match_adapter_with_glob("*-4", "gpt-3").is_none());
    }

    #[test]
    fn glob_match_non_matching_comprehensive() {
        // Comprehensive non-matching pattern tests
        assert!(match_adapter_with_glob("claude-*", "gpt-4").is_none());
        assert!(match_adapter_with_glob("claude-*", "opus-4").is_none());
        assert!(match_adapter_with_glob("claude-*", "claude").is_none()); // No suffix

        assert!(match_adapter_with_glob("gpt-?", "gpt-").is_none()); // Need one char
        assert!(match_adapter_with_glob("gpt-?", "gpt-12").is_none()); // Too many chars
        assert!(match_adapter_with_glob("gpt-?", "claude-4").is_none()); // Wrong prefix

        assert!(match_adapter_with_glob("^exact$", "exact").is_none()); // ^ not special in glob
        assert!(match_adapter_with_glob("model.*", "model-xyz").is_none()); // . not special in glob
    }

    #[test]
    fn glob_match_real_world_model_names() {
        // Test with real-world model name patterns
        assert!(match_adapter_with_glob("claude-sonnet-*", "claude-sonnet-4-6").is_some());
        assert!(match_adapter_with_glob("claude-sonnet-*", "claude-sonnet-4-5-20251001").is_some());

        assert!(match_adapter_with_glob("gpt-*", "gpt-4").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-4-turbo").is_some());
        assert!(match_adapter_with_glob("gpt-*", "gpt-3.5-turbo").is_some());

        assert!(match_adapter_with_glob("*-turbo", "gpt-4-turbo").is_some());
        assert!(match_adapter_with_glob("*-turbo", "claude-sonnet-turbo").is_some());
        assert!(match_adapter_with_glob("*-turbo", "gpt-4").is_none());

        // Provider/model patterns
        assert!(match_adapter_with_glob("anthropic/*", "anthropic/claude-sonnet").is_some());
        assert!(match_adapter_with_glob("openai/*", "openai/gpt-4").is_some());
        assert!(match_adapter_with_glob("anthropic/*", "openai/gpt-4").is_none());
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for match_glob
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn match_glob_with_wildcard() {
        // Single asterisk wildcard
        assert!(match_glob("claude-*", "claude-sonnet-4-6"));
        assert!(match_glob("claude-*", "claude-opus-4-6"));
        assert!(match_glob("claude-*", "claude-haiku-4-5"));
        assert!(match_glob("gpt-*", "gpt-4"));
        assert!(match_glob("gpt-*", "gpt-3.5"));

        // Non-matching patterns
        assert!(!match_glob("claude-*", "gpt-4"));
        assert!(!match_glob("gpt-*", "claude-sonnet"));
    }

    #[test]
    fn match_glob_catchall() {
        // Catch-all pattern with *
        assert!(match_glob("*", "any-model"));
        assert!(match_glob("*", "claude-sonnet-4-6"));
        assert!(match_glob("*", "gpt-4"));
        assert!(match_glob("*", "provider/model"));
    }

    #[test]
    fn match_glob_double_wildcard() {
        // Double asterisk matches any characters including slashes
        assert!(match_glob("**", "any-model"));
        assert!(match_glob("**", "provider/model"));
        assert!(match_glob("**", "nested/path/model"));
    }

    #[test]
    fn match_glob_question_mark() {
        // Question mark matches exactly one character
        assert!(match_glob("gpt-?", "gpt-4"));
        assert!(match_glob("gpt-?", "gpt-3"));
        assert!(!match_glob("gpt-?", "gpt-35")); // Two chars
        assert!(!match_glob("gpt-?", "gpt-")); // Zero chars after dash
        assert!(!match_glob("gpt-?", "gpt-4.5")); // Has extra chars
    }

    #[test]
    fn match_glob_character_class() {
        // Character class [a-z]
        assert!(match_glob("claude-[a-z]*", "claude-sonnet"));
        assert!(match_glob("claude-[a-z]*", "claude-opus"));
        assert!(match_glob("gpt-[0-9]", "gpt-4"));
        assert!(!match_glob("gpt-[0-9]", "gpt-35")); // Two digits
    }

    #[test]
    fn match_glob_empty_pattern() {
        // Empty pattern returns false
        assert!(!match_glob("", "model"));
        assert!(!match_glob("", ""));
    }

    #[test]
    fn match_glob_empty_model_name() {
        // Empty model name returns false
        assert!(!match_glob("pattern", ""));
        assert!(!match_glob("", ""));
        assert!(!match_glob("*", ""));
    }

    #[test]
    fn match_glob_exact_string() {
        // Exact string match without wildcards
        assert!(match_glob("claude-sonnet-4-6", "claude-sonnet-4-6"));
        assert!(match_glob("gpt-4", "gpt-4"));
        assert!(!match_glob("claude-sonnet-4-6", "claude-opus-4-6"));
        assert!(!match_glob("gpt-4", "gpt-3.5"));
    }

    #[test]
    fn match_glob_complex_patterns() {
        // More complex patterns
        assert!(match_glob("*-sonnet-*", "claude-sonnet-4-6"));
        assert!(match_glob("*-sonnet-*", "anthropic-sonnet-4-6"));
        assert!(!match_glob("*-sonnet-*", "claude-opus-4-6"));

        assert!(match_glob("claude-*-4-*", "claude-sonnet-4-6"));
        assert!(match_glob("claude-*-4-*", "claude-opus-4-6"));
        assert!(!match_glob("claude-*-4-*", "claude-sonnet-3-5"));
    }

    #[test]
    fn match_glob_with_slashes() {
        // Patterns with path separators
        assert!(match_glob("provider/*", "provider/model"));
        assert!(match_glob("provider/*", "provider/gpt-4"));
        assert!(!match_glob("provider/*", "other/model"));

        // Double wildcard with slashes
        assert!(match_glob("provider/**", "provider/model"));
        assert!(match_glob("provider/**", "provider/nested/model"));
    }

    #[test]
    fn match_glob_invalid_pattern() {
        // Invalid glob patterns return false
        assert!(!match_glob("[", "model")); // Unclosed bracket
    }

    #[test]
    fn match_glob_real_world_patterns() {
        // Real-world model routing patterns
        assert!(match_glob("claude-sonnet-*", "claude-sonnet-4-6"));
        assert!(match_glob("claude-opus-*", "claude-opus-4-6"));
        assert!(match_glob("claude-haiku-*", "claude-haiku-4-5"));
        assert!(match_glob("claude-fable-*", "claude-fable-5"));

        // OpenAI models
        assert!(match_glob("gpt-*", "gpt-4"));
        assert!(match_glob("gpt-*", "gpt-3.5-turbo"));

        // Generic catch-all
        assert!(match_glob("*", "any-unknown-model"));
    }

    #[test]
    fn match_glob_case_sensitive() {
        // Glob matching is case-sensitive
        assert!(match_glob("Claude-*", "Claude-Sonnet"));
        assert!(!match_glob("Claude-*", "claude-sonnet"));
        assert!(!match_glob("claude-*", "CLAUDE-SONNET"));
    }

    #[test]
    fn match_glob_with_special_characters() {
        // Model names with special characters
        assert!(match_glob("claude-*", "claude-sonnet_4_6"));
        assert!(match_glob("gpt-*", "gpt-4.turbo"));
        assert!(match_glob("*", "model-with-dashes"));
        assert!(match_glob("*", "model_with_underscores"));
    }

    #[test]
    fn match_glob_multiple_wildcards() {
        // Test multiple wildcards in same pattern
        assert!(match_glob("*-*", "claude-sonnet"));
        assert!(match_glob("*-*", "gpt-4"));
        assert!(!match_glob("*-*", "model")); // No dash

        assert!(match_glob("*-*-*", "claude-sonnet-4"));
        assert!(match_glob("*-*-*", "a-b-c"));
        assert!(!match_glob("*-*-*", "a-b")); // Only two parts
    }

    #[test]
    fn match_glob_trailing_wildcard() {
        // Test trailing wildcards with single asterisk
        assert!(match_glob("claude-*", "claude-sonnet"));
        assert!(match_glob("claude-*", "claude-sonnet-4-6"));
        assert!(!match_glob("claude-*", "claude")); // Need at least one char after dash

        // Test trailing wildcards with double asterisk
        assert!(match_glob("claude/**", "claude/sonnet"));
        assert!(match_glob("claude/**", "claude/sonnet/4"));
        assert!(!match_glob("claude/**", "claude")); // Need slash after claude

        // Test patterns ending with various wildcards
        assert!(match_glob("*-4", "gpt-4"));
        assert!(match_glob("*-4", "claude-sonnet-4"));
        assert!(!match_glob("*-4", "gpt-3"));
    }

    #[test]
    fn match_glob_non_matching_comprehensive() {
        // Comprehensive non-matching pattern tests
        assert!(!match_glob("claude-*", "gpt-4"));
        assert!(!match_glob("claude-*", "opus-4"));
        assert!(!match_glob("claude-*", "claude")); // No suffix

        assert!(!match_glob("gpt-?", "gpt-")); // Need one char
        assert!(!match_glob("gpt-?", "gpt-12")); // Too many chars
        assert!(!match_glob("gpt-?", "claude-4")); // Wrong prefix

        assert!(!match_glob("^exact$", "exact")); // ^ not special in glob
        assert!(!match_glob("model.*", "model-xyz")); // . not special in glob
    }

    #[test]
    fn match_glob_real_world_model_names() {
        // Test with real-world model name patterns
        assert!(match_glob("claude-sonnet-*", "claude-sonnet-4-6"));
        assert!(match_glob("claude-sonnet-*", "claude-sonnet-4-5-20251001"));

        assert!(match_glob("gpt-*", "gpt-4"));
        assert!(match_glob("gpt-*", "gpt-4-turbo"));
        assert!(match_glob("gpt-*", "gpt-3.5-turbo"));

        assert!(match_glob("*-turbo", "gpt-4-turbo"));
        assert!(match_glob("*-turbo", "claude-sonnet-turbo"));
        assert!(!match_glob("*-turbo", "gpt-4"));

        // Provider/model patterns
        assert!(match_glob("anthropic/*", "anthropic/claude-sonnet"));
        assert!(match_glob("openai/*", "openai/gpt-4"));
        assert!(!match_glob("anthropic/*", "openai/gpt-4"));
    }

    #[test]
    fn match_glob_empty_string_variations() {
        // Comprehensive empty string tests
        assert!(!match_glob("*", "")); // Empty model with wildcard
        assert!(!match_glob("**", "")); // Empty model with double wildcard
        assert!(!match_glob("pattern", "")); // Empty model with pattern
        assert!(!match_glob("", "model")); // Empty pattern
        assert!(!match_glob("", "")); // Both empty
    }

    #[test]
    fn match_glob_bracket_patterns() {
        // Test negated character classes
        assert!(match_glob("gpt-[!0-9]", "gpt-a"));
        assert!(match_glob("gpt-[!0-9]", "gpt-x"));
        assert!(!match_glob("gpt-[!0-9]", "gpt-4"));

        // Test ranges
        assert!(match_glob("model-[a-c]", "model-a"));
        assert!(match_glob("model-[a-c]", "model-b"));
        assert!(match_glob("model-[a-c]", "model-c"));
        assert!(!match_glob("model-[a-c]", "model-d"));
    }

    #[test]
    fn first_match_wins_glob_patterns() {
        // Test that rule order matters with glob patterns.
        // Earlier specific rules should win over later general rules.
        let rules = vec![
            // More specific pattern first
            make_rule("claude-sonnet-*", "specific-adapter"),
            // Less specific pattern later
            make_rule("claude-*", "general-adapter"),
            // Catch-all last
            make_rule("*", "catchall"),
        ];

        // Both "claude-sonnet-*" and "claude-*" would match "claude-sonnet-4-6",
        // but the first rule should win.
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("specific-adapter".to_string())
        );

        // Only "claude-*" matches "claude-opus-4-6"
        assert_eq!(
            match_adapter("claude-opus-4-6", &rules, "fallback"),
            Some("general-adapter".to_string())
        );

        // Only "*" matches "gpt-4"
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn rule_order_matters_reversed() {
        // Test the opposite: when general rule comes first, it wins.
        let rules = vec![
            // General pattern first
            make_rule("claude-*", "general-adapter"),
            // More specific pattern later (never reached for claude-* models)
            make_rule("claude-sonnet-*", "specific-adapter"),
        ];

        // "claude-*" matches first and wins, even though "claude-sonnet-*"
        // would also match and is more specific.
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("general-adapter".to_string())
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Additional comprehensive edge case tests
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_rules_with_non_empty_default() {
        // Empty rules list should return default adapter when provided
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(
            match_adapter("any-model", &rules, "default-adapter"),
            Some("default-adapter".to_string())
        );
    }

    #[test]
    fn empty_rules_with_empty_default() {
        // Empty rules list with empty default should return None
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(match_adapter("any-model", &rules, ""), None);
    }

    #[test]
    fn single_rule_no_match_with_default() {
        // Single rule that doesn't match should return default
        let rules = vec![make_rule("sonnet.*", "sonnet-adapter")];
        assert_eq!(
            match_adapter("gpt-4", &rules, "default-adapter"),
            Some("default-adapter".to_string())
        );
    }

    #[test]
    fn single_rule_no_match_empty_default() {
        // Single rule that doesn't match with empty default should return None
        let rules = vec![make_rule("sonnet.*", "sonnet-adapter")];
        assert_eq!(match_adapter("gpt-4", &rules, ""), None);
    }

    #[test]
    fn all_rules_invalid_empty_default() {
        // All invalid patterns with empty default should return None
        let rules = vec![
            make_rule("[invalid(regex", "bad-adapter"),
            make_rule("(unclosed", "also-bad"),
        ];
        assert_eq!(match_adapter("any-model", &rules, ""), None);
    }

    #[test]
    fn all_rules_invalid_with_default() {
        // All invalid patterns with default should return default
        let rules = vec![
            make_rule("[invalid(regex", "bad-adapter"),
            make_rule("(unclosed", "also-bad"),
        ];
        assert_eq!(
            match_adapter("any-model", &rules, "default-adapter"),
            Some("default-adapter".to_string())
        );
    }

    #[test]
    fn regex_plus_glob_combination_first_match_wins() {
        // Test combination of regex and glob patterns with first-match-wins
        let rules = vec![
            make_rule("^claude-sonnet", "regex-adapter"),
            make_rule("claude-*", "glob-adapter"),
            make_rule("*", "catchall"),
        ];

        // Both regex and glob would match, but regex (first) wins
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("regex-adapter".to_string())
        );

        // "^claude-sonnet" has no trailing `$`, so it's a prefix match, not
        // an exact match — it still matches "claude-sonnet-4-6", and since
        // the regex rule is listed first, it wins over the glob rule.
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("regex-adapter".to_string())
        );

        // Only catchall matches
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn glob_plus_regex_combination_first_match_wins() {
        // Test combination where glob comes first
        let rules = vec![
            make_rule("claude-*", "glob-adapter"),
            make_rule("^claude-sonnet", "regex-adapter"),
            make_rule("*", "catchall"),
        ];

        // Both glob and regex would match "claude-sonnet", but glob (first) wins
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("glob-adapter".to_string())
        );

        // Only glob matches
        assert_eq!(
            match_adapter("claude-opus", &rules, "fallback"),
            Some("glob-adapter".to_string())
        );

        // Only catchall matches
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn multiple_regex_patterns_first_match_wins() {
        // Multiple regex patterns where more than one could match
        let rules = vec![
            make_rule("claude.*", "first-regex"),
            make_rule("claude-sonnet.*", "second-regex"),
            make_rule(".*", "catchall-regex"),
        ];

        // First regex matches and wins (even though second also matches)
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("first-regex".to_string())
        );

        // Third regex matches
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("catchall-regex".to_string())
        );
    }

    #[test]
    fn multiple_glob_patterns_first_match_wins() {
        // Multiple glob patterns where more than one could match
        let rules = vec![
            make_rule("*", "catchall"),
            make_rule("claude-*", "claude-adapter"),
            make_rule("*sonnet*", "sonnet-adapter"),
        ];

        // First glob matches and wins (even though others would also match)
        assert_eq!(
            match_adapter("claude-sonnet-4-6", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn invalid_glob_pattern_returns_none_on_empty_default() {
        // Invalid glob pattern should skip rule and eventually return None
        let rules = vec![make_rule("**invalid**", "bad-adapter")];
        assert_eq!(match_adapter("any-model", &rules, ""), None);
    }

    #[test]
    fn invalid_regex_pattern_continues_to_next_rule() {
        // Invalid regex should be skipped and continue to next valid rule
        let rules = vec![
            make_rule("[unclosed", "bad-adapter"),
            make_rule("gpt-.*", "good-adapter"),
        ];

        // First rule is invalid and skipped, second matches
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("good-adapter".to_string())
        );
    }

    #[test]
    fn mixed_valid_and_invalid_patterns() {
        // Mix of valid and invalid patterns throughout the list
        let rules = vec![
            make_rule("[invalid1", "bad1"),
            make_rule("claude-.*", "claude-adapter"),
            make_rule("[invalid2", "bad2"),
            make_rule("gpt-.*", "gpt-adapter"),
            make_rule("[invalid3", "bad3"),
        ];

        // Should skip invalid and match claude
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );

        // Should skip invalid and match gpt
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("gpt-adapter".to_string())
        );

        // Should skip all invalid and return default
        assert_eq!(
            match_adapter("other", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn default_adapter_with_special_characters() {
        // Test that default adapter with special characters is preserved
        let rules: Vec<RoutingRule> = vec![];
        let default = "adapter-with_special.chars/123";
        assert_eq!(
            match_adapter("any-model", &rules, default),
            Some(default.to_string())
        );
    }

    #[test]
    fn default_adapter_with_unicode() {
        // Test that default adapter with unicode is preserved
        let rules: Vec<RoutingRule> = vec![];
        let default = "adapter-🎯-中文-テスト";
        assert_eq!(
            match_adapter("any-model", &rules, default),
            Some(default.to_string())
        );
    }

    #[test]
    fn empty_default_with_whitespace() {
        // Test that whitespace-only default is treated as non-empty
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(
            match_adapter("any-model", &rules, "   "),
            Some("   ".to_string())
        );
    }

    #[test]
    fn no_match_empty_vs_whitespace_default() {
        // Distinguish between empty and whitespace-only defaults
        let rules = vec![make_rule("sonnet.*", "adapter")];

        // Empty default returns None
        assert_eq!(match_adapter("gpt-4", &rules, ""), None);

        // Whitespace default returns Some with whitespace
        assert_eq!(
            match_adapter("gpt-4", &rules, "  "),
            Some("  ".to_string())
        );
    }

    #[test]
    fn complex_regex_pattern_with_alternation() {
        // Test regex with alternation (|)
        let rules = vec![make_rule("(sonnet|opus|haiku)-.*", "claude-adapter")];

        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("opus-4-6", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("haiku-4-5", &rules, "fallback"),
            Some("claude-adapter".to_string())
        );
        assert_eq!(
            match_adapter("fable-5", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn complex_glob_pattern_with_character_class() {
        // Test glob with character class ranges
        let rules = vec![make_rule("model-[0-9]-*", "numeric-adapter")];

        assert_eq!(
            match_adapter("model-4-xyz", &rules, "fallback"),
            Some("numeric-adapter".to_string())
        );
        assert_eq!(
            match_adapter("model-9-abc", &rules, "fallback"),
            Some("numeric-adapter".to_string())
        );
        // 'a' is not in [0-9]
        assert_eq!(
            match_adapter("model-a-xyz", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn regex_anchors_with_catchall() {
        // Test that anchored regex doesn't match partial matches
        let rules = vec![
            make_rule("^sonnet$", "exact"),
            make_rule("*", "catchall"),
        ];

        // Exact match
        assert_eq!(
            match_adapter("sonnet", &rules, "fallback"),
            Some("exact".to_string())
        );

        // Partial match - regex doesn't match, catchall does
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn interleaved_regex_and_glob_first_match_wins() {
        // Interleave regex and glob patterns to test first-match semantics
        let rules = vec![
            make_rule("claude.*", "regex1"),
            make_rule("gpt-*", "glob1"),
            make_rule("sonnet.*", "regex2"),
            make_rule("*", "glob2"),
        ];

        // First regex matches
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("regex1".to_string())
        );

        // First glob matches (regex1 and regex2 don't)
        assert_eq!(
            match_adapter("gpt-4", &rules, "fallback"),
            Some("glob1".to_string())
        );

        // Second regex matches
        assert_eq!(
            match_adapter("sonnet-4-6", &rules, "fallback"),
            Some("regex2".to_string())
        );

        // Last glob matches everything else
        assert_eq!(
            match_adapter("gemini-pro", &rules, "fallback"),
            Some("glob2".to_string())
        );
    }

    #[test]
    fn stress_test_many_rules_first_match_wins() {
        // Test with many rules to ensure early exit works correctly
        let rules = vec![
            make_rule("aaa", "adapter-1"),
            make_rule("bbb", "adapter-2"),
            make_rule("ccc", "adapter-3"),
            make_rule("ddd", "adapter-4"),
            make_rule("eee", "adapter-5"),
            make_rule("fff", "adapter-6"),
            make_rule("ggg", "adapter-7"),
            make_rule("hhh", "adapter-8"),
            make_rule("iii", "adapter-9"),
            make_rule("jjj", "adapter-10"),
        ];

        // First rule matches
        assert_eq!(
            match_adapter("aaa", &rules, "fallback"),
            Some("adapter-1".to_string())
        );

        // Middle rule matches
        assert_eq!(
            match_adapter("eee", &rules, "fallback"),
            Some("adapter-5".to_string())
        );

        // Last rule matches
        assert_eq!(
            match_adapter("jjj", &rules, "fallback"),
            Some("adapter-10".to_string())
        );

        // No match, use default
        assert_eq!(
            match_adapter("zzz", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn case_sensitive_matching_respected() {
        // Ensure matching is case-sensitive for both regex and glob
        let rules = vec![
            make_rule("Claude-Sonnet", "exact-case"),
            make_rule("claude-*", "glob-lowercase"),
        ];

        // Exact case match
        assert_eq!(
            match_adapter("Claude-Sonnet", &rules, "fallback"),
            Some("exact-case".to_string())
        );

        // Glob matches lowercase only
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("glob-lowercase".to_string())
        );

        // Uppercase doesn't match lowercase glob
        assert_eq!(
            match_adapter("CLAUDE-SONNET", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn default_parameter_used_correctly_on_none_return() {
        // Verify that when match_adapter returns None, the caller can use default
        // This tests the contract: None means "use your own default"
        let rules = vec![make_rule("sonnet.*", "adapter")];

        // When we pass empty default, we get None
        let result = match_adapter("gpt-4", &rules, "");
        assert!(result.is_none());

        // Caller's responsibility to handle None and use their own default
        let final_adapter = result.unwrap_or_else(|| "caller-default".to_string());
        assert_eq!(final_adapter, "caller-default");

        // When we pass a non-empty default, we get Some(default)
        let result2 = match_adapter("gpt-4", &rules, "fallback");
        assert_eq!(result2, Some("fallback".to_string()));
    }

    #[test]
    fn empty_model_with_empty_rules_empty_default() {
        // Edge case: empty model name, empty rules, empty default
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(match_adapter("", &rules, ""), None);
    }

    #[test]
    fn empty_model_with_empty_rules_non_empty_default() {
        // Edge case: empty model name, empty rules, non-empty default
        let rules: Vec<RoutingRule> = vec![];
        assert_eq!(
            match_adapter("", &rules, "default"),
            Some("default".to_string())
        );
    }

    #[test]
    fn empty_model_with_catchall_rule() {
        // Empty model name should match catchall pattern
        let rules = vec![make_rule("*", "catchall")];
        assert_eq!(
            match_adapter("", &rules, "fallback"),
            Some("catchall".to_string())
        );
    }

    #[test]
    fn escaped_regex_special_chars() {
        // Test escaped regex special characters are treated literally
        let rules = vec![make_rule(r"model\.x", "exact-match")];

        // Should match exact "model.x"
        assert_eq!(
            match_adapter("model.x", &rules, "fallback"),
            Some("exact-match".to_string())
        );

        // Should NOT match "model-x" (dot is literal, not wildcard)
        assert_eq!(
            match_adapter("model-x", &rules, "fallback"),
            Some("fallback".to_string())
        );
    }
}
