# Task: Fix and verify repeat_interval_skips_max_depth_beads test

## Date
2026-07-07

## Findings

### Test Status
The `repeat_interval_skips_max_depth_beads` test **already passes**. No code fix was required.

```
test mitosis::tests::repeat_interval_skips_mitosis_depth_beads ... ok
test mitosis::tests::repeat_interval_skips_max_depth_beads ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured
```

### Duplicate Tests Identified

There are two essentially identical tests in `src/mitosis/mod.rs`:

1. **`repeat_interval_skips_mitosis_depth_beads`** (lines 1112-1150)
   - Verifies beads with `mitosis-depth:1` label are skipped at repeat interval ticks

2. **`repeat_interval_skips_max_depth_beads`** (lines 1204-1244)
   - Verifies the same behavior with slightly different comments

Both tests:
- Use `repeat_interval: 50`
- Test a bead with `failure-count:51` and `mitosis-depth:1` labels
- Verify the bead is skipped despite being at a repeat tick

### Implementation Verified

The logic in `MitosisEvaluator::evaluate()` (lines 124-133) correctly prevents recursive mitosis:

```rust
let has_mitosis_depth_label = bead.labels.iter().any(|l| l.starts_with("mitosis-depth:"));
let is_repeat_tick = failure_count > 1 && (failure_count - 1) % self.config.repeat_interval == 0;

// Fire at first failure OR at repeat interval ticks (if not a mitosis child)
failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)
```

### Verification Completed

- ✅ `repeat_interval_skips_max_depth_beads` test passes
- ✅ `repeat_interval_skips_mitosis_depth_beads` test passes
- ✅ No clippy warnings
- ✅ Code properly formatted

### Work Performed

1. Ran both tests and verified they pass
2. Ran `cargo clippy --all-targets -- -D warnings` - no warnings
3. Ran `cargo fmt` - fixed formatting in `src/config/mod.rs` (unrelated to this test)

### Conclusion

No fix was needed for the `repeat_interval_skips_max_depth_beads` test. The test correctly verifies that mitosis child beads (with `mitosis-depth:1` label) are excluded from repeat interval evaluation, preventing recursive splitting.
