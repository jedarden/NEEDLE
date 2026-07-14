# bf-3poz: Implement first-match-wins semantics

## Verification Summary

First-match-wins iteration semantics are **already correctly implemented** in the `match_adapter` function. No code changes needed.

## Implementation Verification

### Location: `src/routing.rs` lines 236-263

The `match_adapter` function implements first-match-wins semantics:

```rust
pub fn match_adapter(model: &str, rules: &[RoutingRule], default: &str) -> Option<String> {
    // Compile rules and test in order (first match wins).
    for rule in rules {
        match CompiledRule::from_rule(rule) {
            Ok(compiled) => {
                if compiled.matches(model) {
                    return Some(compiled.adapter.clone());  // ← Early return on first match
                }
            }
            // ... error handling ...
        }
    }
    // ... default fallback ...
}
```

**Key behavior:**
1. Rules are iterated in config order (via `for rule in rules` slice iteration)
2. First matching rule triggers **early return** (line 242)
3. Remaining rules are **not checked** after first match
4. Enables more-specific patterns to precede less-specific ones

## Test Coverage

### Unit test: `first_match_wins` (line 520)

```rust
#[test]
fn first_match_wins() {
    let rules = vec![
        make_rule("claude.*", "first-adapter"),
        make_rule("claude-sonnet.*", "second-adapter"), // Never matched.
        make_rule("*", "catchall"),
    ];

    // First rule matches ("claude-sonnet-4-6" matches "claude.*")
    assert_eq!(
        match_adapter("claude-sonnet-4-6", &rules, "fallback"),
        Some("first-adapter".to_string())
    );
    // Third rule matches (catch-all)
    assert_eq!(
        match_adapter("other-model", &rules, "fallback"),
        Some("catchall".to_string())
    );
}
```

**Demonstrates:**
- Model "claude-sonnet-4-6" matches BOTH "claude.*" AND "claude-sonnet.*"
- First rule wins (returns "first-adapter", not "second-adapter")
- Rule order matters

## Test Results

```
cargo test routing::tests::first_match_wins --lib
test routing::tests::first_match_wins ... ok

cargo test routing::tests --lib
test result: ok. 77 passed; 0 failed; 0 ignored
```

## Acceptance Criteria Status

- ✅ match_adapter returns on first matching rule, stops iteration
- ✅ Unit test demonstrates rule order matters (first match wins)
- ✅ cargo test passes (77 routing tests pass)

## Related Beads

- `bf-52o9i` (closed) - Verified ordered rule iteration in match_adapter
- `bf-1h27o` (closed) - Added test for first-match-wins semantics

## Conclusion

No implementation changes required. The first-match-wins semantics were already correctly implemented in the original `match_adapter` function. This bead served to verify and document the existing behavior.
