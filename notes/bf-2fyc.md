# repeat_interval_skips_max_depth_beads Test Documentation

## Test Location
`src/mitosis/mod.rs` lines 1203-1244

## What It Tests
Validates that beads with the `mitosis-depth:1` label are excluded from repeat_interval mitosis evaluation, even when the failure count is at a repeat tick.

## Configuration
- `repeat_interval: 50` - triggers at counts 1, 51, 101, 151, ...
- `first_failure_only: false` - using repeat_interval mode
- `enabled: true`

## Test Setup
```rust
// Bead with:
// - failure-count:51 (at repeat tick: 1+50=51)
// - mitosis-depth:1 label (child bead from mitosis)
let store = MockStore::new().with_labels(vec![
    "failure-count:51".to_string(),
    "mitosis-depth:1".to_string(),
]);
let mut bead = test_bead();
bead.labels = vec![
    "failure-count:51".to_string(),
    "mitosis-depth:1".to_string(),
];
```

## Expected Behavior
The test expects `MitosisResult::Skipped` because:
1. `is_repeat_tick = (51 - 1) % 50 == 0` → `true`
2. `has_mitosis_depth_label = true` (bead has mitosis-depth:1)
3. Fire condition: `failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)`
   - `51 == 1` → `false`
   - `true && !true` → `false`
   - Overall: `false` → skip

## Assertion
```rust
assert!(
    matches!(result, MitosisResult::Skipped { .. }),
    "bead with mitosis-depth:1 should be skipped even at repeat tick (failure_count=51)"
);
```

## Implementation Reference
Lines 124-133 in `evaluate()`:
```rust
let has_mitosis_depth_label =
    bead.labels.iter().any(|l| l.starts_with("mitosis-depth:"));
let is_repeat_tick =
    failure_count > 1 && (failure_count - 1) % self.config.repeat_interval == 0;

// Fire at first failure OR at repeat interval ticks (if not a mitosis child)
failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)
```

## Purpose
Prevents infinite recursive mitosis by ensuring child beads (created with `mitosis-depth:1` label) are not themselves subject to repeat_interval mitosis evaluation.
