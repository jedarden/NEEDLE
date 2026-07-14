//! Routing rule matcher for model-to-adapter dispatch.
//!
//! Provides pattern matching logic for routing rules: regex/glob patterns
//! against model names, first-match-wins semantics, and default fallback.

use crate::config::RoutingRule;
use std::sync::Arc;

/// Compiled routing rule with cached regex for efficient matching.
#[derive(Debug, Clone)]
struct CompiledRule {
    /// Compiled regex matcher.
    matcher: Arc<regex::Regex>,
    /// Adapter to use on match.
    adapter: String,
}

impl CompiledRule {
    /// Compile a routing rule into an efficient matcher.
    ///
    /// Supports both regex patterns (any valid regex) and glob-style patterns:
    /// - `*` matches any single path segment (non-greedy `[^/]+`)
    /// - `**` matches zero or more path segments (`.+?` with greedy quantifier)
    /// - Literal `*` or `**` must be escaped as `\*` or `\*\*`
    fn from_rule(rule: &RoutingRule) -> Result<Self, regex::Error> {
        let pattern = &rule.match_model;
        let adapter = rule.adapter.clone();

        // Convert glob patterns to regex if needed.
        // If the pattern contains * or ** without proper regex escaping,
        // treat it as a glob pattern and convert.
        let regex_pattern = if needs_glob_conversion(pattern) {
            convert_glob_to_regex(pattern)
        } else {
            pattern.clone()
        };

        let matcher = regex::Regex::new(&regex_pattern)?;

        Ok(CompiledRule {
            matcher: Arc::new(matcher),
            adapter,
        })
    }

    /// Test if a model name matches this rule.
    fn matches(&self, model: &str) -> bool {
        self.matcher.is_match(model)
    }
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

/// Convert glob-style pattern to regex.
///
/// Rules:
/// - `*` alone → `.*` (match anything, catch-all)
/// - `*` in other contexts → `[^/]+` (match any single segment, no slashes)
/// - `**` → `.*` (match any characters including slashes, greedy)
/// - Escaped `\*` and `\*\*` treated literally
/// - No implicit start anchor for glob patterns (unlike regex patterns)
fn convert_glob_to_regex(glob: &str) -> String {
    // Special case: * and ** should match anything (including empty).
    if glob == "*" || glob == "**" {
        return "^.*$".to_string();
    }

    let mut result = String::new();
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '\\' => {
                // Escaped character - pass through the backslash and the character.
                // This ensures that \* in the glob becomes \* in the regex (literal asterisk).
                if i + 1 < chars.len() {
                    result.push('\\');
                    result.push(chars[i + 1]);
                    i += 2;
                } else {
                    // Trailing backslash - treat literally.
                    result.push('\\');
                    i += 1;
                }
            }
            '*' => {
                // Check for **.
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // ** matches any characters including slashes.
                    result.push_str(".*");
                    i += 2;
                } else {
                    // * matches any non-slash characters.
                    result.push_str("[^/]+");
                    i += 1;
                }
            }
            c => {
                // Pass through regex metacharacters literally - regex::Regex::new
                // will handle escaping. We only need to escape backslashes for
                // the glob conversion.
                result.push(c);
                i += 1;
            }
        }
    }

    // Only add end anchor, not start anchor (glob patterns match substrings).
    format!("{}$", result)
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
        // ** matches any characters including slashes.
        let rules = vec![make_rule("claude**", "claude-print")];
        assert_eq!(
            match_adapter("claude-sonnet", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude/sonnet", &rules, "fallback"),
            Some("claude-print".to_string())
        );
        assert_eq!(
            match_adapter("claude/any/nested/path", &rules, "fallback"),
            Some("claude-print".to_string())
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
            make_rule("opus*", "opus-adapter"), // Glob style.
            make_rule("haiku*", "haiku-adapter"),
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
    fn convert_glob_to_regex_single_asterisk() {
        assert_eq!(convert_glob_to_regex("*"), "^.*$");
        assert_eq!(convert_glob_to_regex("claude-*"), "claude-[^/]+$");
        assert_eq!(convert_glob_to_regex("provider/*"), "provider/[^/]+$");
    }

    #[test]
    fn convert_glob_to_regex_double_asterisk() {
        assert_eq!(convert_glob_to_regex("**"), "^.*$");
        assert_eq!(convert_glob_to_regex("provider/**"), "provider/.*$");
    }

    #[test]
    fn convert_glob_to_regex_escaped() {
        assert_eq!(convert_glob_to_regex(r"model\*"), r"model\*$");
        assert_eq!(convert_glob_to_regex(r"model\*\*"), r"model\*\*$");
    }

    #[test]
    fn convert_glob_to_regex_mixed() {
        // Mix of glob wildcards and literal characters.
        assert_eq!(convert_glob_to_regex("a*b"), "a[^/]+b$");
        assert_eq!(convert_glob_to_regex("a**b"), "a.*b$");
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
}
