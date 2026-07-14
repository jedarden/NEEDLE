# Bead bf-5axym: Early Return on First Match

## Finding

The `match_adapter` function in `src/routing.rs` **already implements early return on first match**. The implementation is at line 236-263:

```rust
pub fn match_adapter(model: &str, rules: &[RoutingRule], default: &str) -> Option<String> {
    // Compile rules and test in order (first match wins).
    for rule in rules {
        match CompiledRule::from_rule(rule) {
            Ok(compiled) => {
                if compiled.matches(model) {
                    return Some(compiled.adapter.clone());  // <-- EARLY RETURN at line 242
                }
            }
            Err(e) => {
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
```

## Verification

All acceptance criteria are already met:

1. ✅ `match_adapter` returns on first matching rule (line 242)
2. ✅ No further rules are checked after a match (early return exits the for loop)
3. ✅ `cargo test` passes for routing tests
4. ✅ Early return is clearly visible in code (top-level if statement, not nested)

## Tests Added

Added 4 baseline tests to `src/worker/mod.rs` to verify and document this behavior:
- `apply_routing_rules_baseline_first_match_stops_evaluation` - Verifies evaluation stops after first match
- `apply_routing_rules_baseline_invalid_then_valid` - Verifies invalid patterns are skipped
- `apply_routing_rules_baseline_multiple_rules_same_adapter` - Verifies first match is reported
- `apply_routing_rules_baseline_three_rules_all_match` - Verifies first of three matching rules wins

All tests pass, confirming the early return behavior is working correctly.
