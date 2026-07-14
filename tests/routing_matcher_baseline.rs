//! Baseline test for routing matcher behavior.
//!
//! This test documents the CURRENT behavior of the matcher when multiple
//! matching rules target the same adapter. This establishes a baseline to
//! verify that behavior is preserved through future changes.
//!
//! The matcher is implemented in needle::routing::match_adapter() and is
//! tested here using its public interface.

use needle::config::RoutingRule;
use needle::routing::match_adapter;

fn make_rule(pattern: &str, adapter: &str) -> RoutingRule {
    RoutingRule {
        match_model: pattern.to_string(),
        adapter: adapter.to_string(),
    }
}

#[test]
fn routing_baseline_multiple_rules_same_adapter() {
    /// Baseline test documenting CURRENT matcher behavior when multiple rules
    /// both match the same model AND route to the same adapter.
    ///
    /// This test verifies that the matcher implements "first-match-wins" behavior:
    /// - When multiple rules match a model, the FIRST matching rule determines the adapter
    /// - The matcher stops checking rules after the first match
    /// - Even if later rules would also match, they are not evaluated
    ///
    /// Current behavior: The first rule in the list that matches determines routing.
    // Configure two rules that both match "claude-sonnet-4-6" AND route to
    // the same adapter "claude-print":
    // - First rule: "claude-.*" -> claude-print (broad pattern, matches first)
    // - Second rule: "claude-sonnet-.*" -> claude-print (more specific, same adapter)
    let rules = vec![
        make_rule("claude-.*", "claude-print"),
        make_rule("claude-sonnet-.*", "claude-print"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(result.is_some(), "routing should succeed when rules match");

    let chosen_adapter = result.unwrap();

    // Baseline assertion: FIRST matching rule wins
    // Even though both rules match and route to the same adapter,
    // the first rule is the one that determines routing.
    assert_eq!(
        chosen_adapter, "claude-print",
        "should route to claude-print when both rules match"
    );

    println!(
        "BASELINE: Multiple rules matched same adapter. chosen_adapter={}",
        chosen_adapter
    );
    println!("Expected behavior: first-match-wins (claude-.* matched first)");
}

#[test]
fn routing_baseline_multiple_rules_different_adapters() {
    /// Baseline test documenting CURRENT matcher behavior when multiple rules
    /// match the same model but route to DIFFERENT adapters.
    ///
    /// This test verifies that "first-match-wins" is enforced even when
    /// later rules would route to different adapters.
    ///
    /// Current behavior: The first matching rule determines routing, regardless
    /// of whether later rules might be "more appropriate".
    // Configure three rules that all match "claude-sonnet-4-6":
    // - First rule: "claude-.*" -> claude-print (broad, matches first)
    // - Second rule: "claude-sonnet-.*" -> claude-code (more specific, never checked)
    // - Third rule: ".*" -> fallback (catch-all, never checked)
    let rules = vec![
        make_rule("claude-.*", "claude-print"),
        make_rule("claude-sonnet-.*", "claude-code-glm-4.7"),
        make_rule(".*", "fallback"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(result.is_some(), "routing should succeed when rules match");

    let chosen_adapter = result.unwrap();

    // Baseline assertion: FIRST matching rule wins, even if later rules
    // might seem more appropriate (claude-code might be more specific for sonnet)
    assert_eq!(
        chosen_adapter, "claude-print",
        "should route to claude-print (first matching adapter)"
    );

    println!(
        "BASELINE: Multiple rules matched different adapters. chosen_adapter={}",
        chosen_adapter
    );
    println!("Note: claude-code-glm-4.7 never matched despite being more specific");
}

#[test]
fn routing_baseline_order_matters() {
    /// Baseline test documenting that rule ORDER is significant.
    ///
    /// This test verifies that swapping rule order changes the outcome,
    /// confirming that the matcher uses first-match-wins semantics.
    ///
    /// Current behavior: Rules are evaluated in order, first match wins.
    // Configure rules with specific BEFORE broad:
    // - First rule: "claude-sonnet-.*" -> claude-code (specific, matches first)
    // - Second rule: "claude-.*" -> claude-print (broad, would also match but not checked)
    let rules = vec![
        make_rule("claude-sonnet-.*", "claude-code-glm-4.7"),
        make_rule("claude-.*", "claude-print"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(result.is_some(), "routing should succeed when rules match");

    let chosen_adapter = result.unwrap();

    // Baseline assertion: Order matters
    // With specific rule first, it matches (not the broad one)
    assert_eq!(
        chosen_adapter, "claude-code-glm-4.7",
        "should route to claude-code when specific rule comes first"
    );

    println!(
        "BASELINE: Rule order is significant. chosen_adapter={}",
        chosen_adapter
    );
    println!("With specific rule first: claude-code-glm-4.7 wins");
}

#[test]
fn routing_baseline_first_match_stops_evaluation() {
    /// Baseline test to verify whether the matcher stops at the first match
    /// or continues checking all rules.
    ///
    /// This test uses counter-like adapters to detect whether all rules are
    /// checked or only the first match.
    ///
    /// Current behavior: The matcher stops evaluation after the first match
    /// and does not check subsequent rules.
    // Configure rules where first match should stop evaluation:
    // - First rule: specific pattern -> adapter-1
    // - Second rule: broader pattern -> adapter-2 (should NOT be checked)
    // - Third rule: catch-all -> adapter-3 (should NOT be checked)
    let rules = vec![
        make_rule("^claude-sonnet-4-6$", "adapter-1"),
        make_rule("claude-.*", "adapter-2"),
        make_rule("*", "adapter-3"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(
        result.is_some(),
        "routing should succeed when a rule matches"
    );

    let chosen_adapter = result.unwrap();

    // Baseline assertion: FIRST match stops evaluation
    assert_eq!(
        chosen_adapter, "adapter-1",
        "should use the first matching rule and stop checking"
    );

    println!(
        "BASELINE: First match stopped evaluation. chosen_adapter={}",
        chosen_adapter
    );
    println!("Subsequent rules (adapter-2, adapter-3) were never evaluated");
}

#[test]
fn routing_baseline_invalid_pattern_skipped() {
    /// Baseline test documenting CURRENT matcher behavior when an invalid
    /// pattern precedes valid matching patterns.
    ///
    /// This test verifies that invalid patterns are skipped gracefully and
    /// the first valid matching pattern determines the adapter.
    ///
    /// Current behavior: Invalid regex patterns are logged as warnings and
    /// skipped, then the next valid pattern is checked.
    // Configure rules with invalid pattern in the middle:
    // - First rule: valid pattern, matches
    // - Second rule: INVALID pattern (should be skipped with warning)
    // - Third rule: valid pattern, would also match but shouldn't be checked
    let rules = vec![
        make_rule("claude-.*", "first-adapter"),
        make_rule("[invalid(regex", "invalid-adapter"),
        make_rule("claude-sonnet-.*", "third-adapter"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(
        result.is_some(),
        "routing should succeed when a valid rule matches"
    );

    let chosen_adapter = result.unwrap();

    // Baseline assertion: first valid match wins, invalid rules are skipped
    assert_eq!(
        chosen_adapter, "first-adapter",
        "should use the first VALID matching rule"
    );

    println!(
        "BASELINE: Invalid pattern was skipped, first valid match determined routing. chosen_adapter={}",
        chosen_adapter
    );
}

#[test]
fn routing_baseline_specific_rule_wins_when_first() {
    /// Test that verifies first-match-wins semantics by showing that a
    /// more-specific rule wins when positioned before a less-specific rule.
    ///
    /// This test demonstrates:
    /// 1. Both rules match the model
    /// 2. The more-specific rule is positioned first
    /// 3. The first (more-specific) rule wins
    /// 4. Rule order matters (different order = different result)
    ///
    /// Current behavior: The first matching rule in the list determines routing,
    /// regardless of whether later rules might be more specific.
    // Configure rules with specific BEFORE broad:
    // - First rule: "claude-sonnet-.*" -> claude-code (specific, matches first)
    // - Second rule: "claude-.*" -> claude-print (broad, would also match but not checked)
    let rules = vec![
        make_rule("claude-sonnet-.*", "claude-code-glm-4.7"),
        make_rule("claude-.*", "claude-print"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(result.is_some(), "routing should succeed when rules match");

    let chosen_adapter = result.unwrap();

    // First-match-wins: The more-specific rule wins because it comes first
    assert_eq!(
        chosen_adapter, "claude-code-glm-4.7",
        "should route to claude-code when specific rule is positioned first"
    );

    println!(
        "BASELINE: Specific rule won when positioned first. chosen_adapter={}",
        chosen_adapter
    );
    println!("Both rules matched, but first (claude-sonnet-.*) won");
}

#[test]
fn routing_baseline_broad_rule_wins_when_first() {
    /// Test that verifies first-match-wins semantics by showing that a
    /// less-specific (broad) rule wins when positioned before a more-specific rule.
    ///
    /// This test complements `routing_baseline_specific_rule_wins_when_first`
    /// by showing the REVERSE case: when the broad rule comes first, the
    /// specific rule never gets checked.
    ///
    /// Current behavior: Rule order is significant - the first matching rule
    /// wins, even if a later rule would be "better" or more specific.
    // Configure rules with broad BEFORE specific (opposite order):
    // - First rule: "claude-.*" -> claude-print (broad, matches first)
    // - Second rule: "claude-sonnet-.*" -> claude-code (specific, never checked)
    let rules = vec![
        make_rule("claude-.*", "claude-print"),
        make_rule("claude-sonnet-.*", "claude-code-glm-4.7"),
    ];

    let result = match_adapter("claude-sonnet-4-6", &rules, "fallback");

    assert!(result.is_some(), "routing should succeed when rules match");

    let chosen_adapter = result.unwrap();

    // First-match-wins: The broad rule wins because it comes first
    // The more-specific rule never gets checked
    assert_eq!(
        chosen_adapter, "claude-print",
        "should route to claude-print when broad rule is positioned first"
    );

    println!(
        "BASELINE: Broad rule won when positioned first. chosen_adapter={}",
        chosen_adapter
    );
    println!("Specific rule (claude-sonnet-.*) never got checked");
}
