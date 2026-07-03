# Bead bf-2wcg Verification

## Task
Add `repeat_interval` field to `MitosisConfig` in `src/config/mod.rs`.

## Verification
The field was already present in the codebase at the time of this bead's execution.

**Location:** `src/config/mod.rs` lines 449-455

**Field definition:**
```rust
/// Re-run mitosis every N consecutive failures after the first (0 = disabled).
///
/// Fires at failure_count == 1, 1+N, 1+2N, ...
/// Only when force_failure_threshold == 0.
/// Beads already carrying a mitosis-depth label are skipped.
#[serde(default)]
pub repeat_interval: u32,
```

**Default value:** 0 (set in `MitosisConfig::default()` at line 464)

## Acceptance Criteria
- ✅ Field added to struct with proper docs
- ✅ Default value is 0
- ✅ `cargo fmt` clean (no changes needed)
- ⚠️ `cargo clippy` fails due to pre-existing issues unrelated to this field:
  - `expand_tilde_pathbuf` function dead code (line 2342)
  - Manual string strip instead of `strip_prefix` (line 2333)
  - `&PathBuf` instead of `&Path` in unused function (line 2342)

These clippy errors existed before this bead and are unrelated to the `repeat_interval` field.

## Conclusion
No code changes were required. The field was already implemented exactly as specified.
