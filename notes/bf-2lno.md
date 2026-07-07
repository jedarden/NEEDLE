# Task: Locate and understand repeat_interval_skips_max_depth_beads test

## Test Location

`src/mitosis/mod.rs` — lines 1204-1244

## Purpose

Verifies that mitosis child beads (labeled with `mitosis-depth:1`) are excluded from repeat interval mitosis triggers, even when their failure count reaches a repeat interval tick. This prevents recursive mitosis — child beads created by mitosis should not themselves be split.

## What the Test Verifies

With `repeat_interval: 50`:
- Creates a bead with `mitosis-depth:1` label and `failure-count:51`
- Failure count 51 is a "repeat tick" since `(51-1) % 50 == 0`
- Confirms the bead is skipped despite being at a repeat tick

## Implementation Logic

In `MitosisEvaluator::evaluate()` (lines 124-133):

```rust
let has_mitosis_depth_label = bead.labels.iter().any(|l| l.starts_with("mitosis-depth:"));
let is_repeat_tick = failure_count > 1 && (failure_count - 1) % self.config.repeat_interval == 0;

// Fire at first failure OR at repeat interval ticks (if not a mitosis child)
failure_count == 1 || (is_repeat_tick && !has_mitosis_depth_label)
```

## Dependencies

- `MockStore` — test double for `BeadStore`
- `MitosisConfig` — with `repeat_interval: 50`
- `test_bead()` helper — creates test bead structure
- `create_test_dispatcher()` — minimal dispatcher for signature
- `PromptBuilder` with default config
- `#[tokio::test]` async test runtime
