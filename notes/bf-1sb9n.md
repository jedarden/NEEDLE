# Bead bf-1sb9n: Routing Integration Test Enhancement

## Summary
Added comprehensive first-match-wins semantics tests to the existing routing integration test suite.

## Work Done

### Tests Added
1. `routing_first_match_wins_with_overlapping_patterns` - Verifies that when multiple patterns match a model, the FIRST match wins
2. `routing_first_match_wins_reversed_order` - Confirms that reversing rule order changes the outcome
3. `routing_first_match_wins_with_specific_patterns` - Tests first-match-wins with realistic Anthropic model patterns

### Test Results
- **Total tests**: 24 (21 existing + 3 new)
- **All tests passing**: ✅ 24/24 passed

### Test Coverage
- Anthropic Claude subscription models → claude-print adapter (sonnet, opus, fable, haiku)
- GLM models → default adapter (claude-code-glm-4.7)
- First-match-wins semantics with overlapping patterns
- Workspace override behavior
- Strict vs non-strict modes
- Real-world configuration scenarios
- June 15, 2026 deadline rationale documentation

## Files Modified
- `tests/routing_integration.rs` - Added 3 new tests for first-match-wins behavior

## Verification
```bash
cargo test --test routing_integration
# running 24 tests
# test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
```
