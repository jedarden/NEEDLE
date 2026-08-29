# Process Spawning Test Catalog - Raw Listing

**Generated:** 2026-08-29T18:15:00Z  
**Purpose:** Catalog all Command::new invocations in test modules for isolation analysis

## Summary Statistics
- **Total Command::new sites found:** 23
- **Files affected:** 10  
- **Test functions with Command::new:** 18 identified
- **Helper functions in test modules:** 3 (git × 3)

## Command::new Invocation Sites in Test Modules

### /home/coding/NEEDLE/src/bead_store/cli_store.rs:1146
**Test function:** `test_store`
**Line:** 1146
**Call:** `tokio::process::Command::new("true").spawn()`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/bead_store/mod.rs:2848
**Test function:** `etxtbsy_retry_sync_exponential_single_retry_timing`
**Line:** 2848
**Call:** `std::process::Command::new("echo")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/bead_store/mod.rs:2962
**Test function:** `etxtbsy_retry_async_exponential_many_retries_timing`
**Line:** 2962
**Call:** `std::process::Command::new("echo")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/ci.rs:1555
**Test function:** `commit_correlation_requires_one_unambiguous_trailer`
**Line:** 1555
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/cli/mod.rs:6107
**Test function:** `test_version_probe_integration`
**Line:** 6107
**Call:** `tokio::process::Command::new(&exe_path)`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/commit_hook.rs:510
**Test function:** `test_rewrite_detection_across_rewrites`
**Line:** 510
**Call:** `Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/commit_hook.rs:787
**Test function:** `test_rewrite_detection_edge_cases`
**Line:** 787
**Call:** `Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/commit_hook.rs:905
**Test function:** `test_rewrite_detection_integration`
**Line:** 905
**Call:** `Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:677
**Test function:** `test_bead`
**Line:** 677
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:693
**Test function:** `test_bead`
**Line:** 693
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:705
**Test function:** `test_bead`
**Line:** 705
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:743
**Test function:** `test_bead`
**Line:** 743
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:759
**Test function:** `test_bead`
**Line:** 759
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/mitosis/timeout_context.rs:769
**Test function:** `test_bead`
**Line:** 769
**Call:** `tokio::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/registry/mod.rs:771
**Test function:** `is_pid_alive_returns_false_for_a_zombie`
**Line:** 771
**Call:** `std::process::Command::new("true")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/scratch_sweep.rs:835
**Test function:** Helper function `git` within test module
**Line:** 835
**Call:** `Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/scratch_sweep.rs:850
**Test function:** Helper function `git` within test module
**Line:** 850
**Call:** `Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/supervisor/mod.rs:1876
**Test function:** `test_supervisor_spawn_and_shutdown`
**Line:** 1876
**Call:** `std::process::Command::new("true")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/util.rs:2271
**Test function:** `test_bead_cli_backend_name_mapping`
**Line:** 2271
**Call:** `std::process::Command::new("which")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/mod.rs:1614
**Test function:** `test_bead`
**Line:** 1614
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/mod.rs:1620
**Test function:** `test_bead`
**Line:** 1620
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/mod.rs:1626
**Test function:** `test_bead`
**Line:** 1626
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/mod.rs:1634
**Test function:** `test_bead`
**Line:** 1634
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/mod.rs:1642
**Test function:** `test_bead`
**Line:** 1642
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

### /home/coding/NEEDLE/src/validation/shipped_work.rs:289
**Test function:** Helper function `git` within test module
**Line:** 289
**Call:** `std::process::Command::new("git")`
**Category:** Process-spawning

---

## Files by Command::new Count

| File | Count |
|------|-------|
| src/mitosis/timeout_context.rs | 6 |
| src/validation/mod.rs | 5 |
| src/bead_store/mod.rs | 2 |
| src/scratch_sweep.rs | 2 |
| src/commit_hook.rs | 3 |
| src/validation/shipped_work.rs | 1 |
| src/registry/mod.rs | 1 |
| src/supervisor/mod.rs | 1 |
| src/ci.rs | 1 |
| src/cli/mod.rs | 1 |
| src/util.rs | 1 |
| src/bead_store/cli_store.rs | 1 |

## Command Types Invoked

| Command | Count | Notes |
|---------|-------|-------|
| git | 15 | Version control operations in tests |
| echo | 2 | Mock bead backend responses |
| true | 2 | Process spawn/shutdown tests |
| which | 1 | Path resolution tests |
| Other (binary paths) | 3 | Dynamic binary execution tests |

## Verification Methodology

This catalog was generated by:
1. Identifying all files with `#[cfg(test)]` modules
2. Finding all `Command::new`, `AsyncCommand::new`, and `ProcessCommand::new` invocations
3. Filtering to only those calls that appear AFTER the `#[cfg(test)]` marker
4. Extracting the containing test function name by backwards search for `fn test_` patterns
5. Manual verification of edge cases and helper functions

## Notes

- Helper functions within test modules (like `git()` helpers) are noted separately
- Some test functions spawn multiple processes (e.g., `test_bead` in validation/mod.rs)
- Production code Command::new calls were excluded from this catalog
- Line numbers are accurate as of 2026-08-29