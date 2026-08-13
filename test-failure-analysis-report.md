# Test Failure Analysis - NEEDLE Test Suite

**Date:** 2026-08-13  
**Test Output:** test_full_output.log  
**Total Tests:** 2074  
**Passed:** 2063  
**Failed:** 11  
**Execution Time:** 587.62s

## Failure Categories

### 1. Tilde Expansion Issues (3 failures)

All related to `~` not being expanded to home directory in paths:

#### `config::config_tests::supervisor_config_from_env_expands_tilde`
- **Error:** `assertion left != right failed: left: Some("~/heartbeat.json"), right: Some("~/heartbeat.json")`
- **Expected:** `~` should expand to home directory
- **Actual:** Tilde remains literal in path
- **Location:** `src/config/mod.rs:4996`

#### `config::config_tests::test_config_expand_tildes_missing_home`
- **Error:** `assertion left == right failed: left: "/home/testuser/workspace", right: "~/workspace"`
- **Expected:** Path should remain `~/workspace` when HOME is missing
- **Actual:** Got expanded path instead
- **Location:** `src/config/mod.rs:6632`

#### `config::config_tests::worker_binary_path_tilde_is_expanded`
- **Error:** `tilde was not expanded: "~/bin/needle"`
- **Expected:** Tilde should expand in worker binary path
- **Actual:** Tilde remains literal
- **Location:** `src/config/mod.rs:5901`

**Pattern:** Tilde expansion implementation in config module is broken. Tests expect proper home directory expansion but paths are staying literal.

---

### 2. Dispatch/Activity Detection Issues (2 failures)

#### `dispatch::tests::activity_detection_with_mixed_stdout_stderr`
- **Error:** `assertion failed: result.stderr.contains("errerrererrererrerrerre")`
- **Expected:** stderr should contain specific error pattern
- **Actual:** Pattern not found in stderr output
- **Location:** `src/dispatch/mod.rs:3873`

#### `dispatch::tests::idle_timeout_fires_when_no_activity`
- **Error:** `assertion left == right failed: idle timeout should return exit code 124, left: 0, right: 124`
- **Expected:** Exit code 124 (timeout) when no activity
- **Actual:** Exit code 0 (success) instead
- **Location:** `src/dispatch/mod.rs:3904`

**Pattern:** Activity detection and idle timeout mechanisms not working as expected. Either the detection logic is broken or the test environment doesn't match expectations.

---

### 3. Git Path Filtering Issue (1 failure)

#### `mitosis::timeout_context::tests::git_dirty_paths_filters_untracked`
- **Error:** `assertion failed: dirty_paths.is_empty()`
- **Expected:** Dirty paths should be empty after filtering untracked files
- **Actual:** Array still contains paths
- **Location:** `src/mitosis/timeout_context.rs:700`

**Pattern:** Git untracked file filtering logic is allowing files through that should be filtered out.

---

### 4. Telemetry/File Sink Issues (5 failures)

Multiple file operations in telemetry module are broken:

#### `telemetry::file_sink::tests::test_file_sink_append_mode`
- **Error:** `assertion failed: content.contains("test.event")`
- **Expected:** File should contain "test.event" after append
- **Actual:** Content not found
- **Location:** `src/telemetry/file_sink.rs:716`

#### `telemetry::file_sink::tests::test_file_sink_daily_rotation`
- **Error:** `assertion failed: new_path.to_string_lossy().contains("2099-01-01")`
- **Expected:** Rotated file path should contain date "2099-01-01"
- **Actual:** Path doesn't contain expected date
- **Location:** `src/telemetry/file_sink.rs:567`

#### `telemetry::file_sink::tests::test_file_sink_permissions`
- **Error:** `assertion left == right failed: left: 420, right: 384`
- **Expected:** File permissions 420 (0o644 = rw-r--r--)
- **Actual:** File permissions 384 (0o600 = rw-------)
- **Location:** `src/telemetry/file_sink.rs:689`

#### `telemetry::tests::boot_event_written_to_file_on_telemetry_creation`
- **Error:** `log file should be created`
- **Expected:** Log file created during telemetry initialization
- **Actual:** File doesn't exist
- **Location:** `src/telemetry/mod.rs:6171`

#### `telemetry::tests::file_sink_writes_jsonl`
- **Error:** `should read file: Os { code: 2, kind: NotFound, message: "No such file or directory" }`
- **Expected:** JSONL file should be written and readable
- **Actual:** File doesn't exist
- **Location:** `src/telemetry/mod.rs:4806`

**Pattern:** Telemetry file sink is completely broken - file creation, rotation, permissions, and content writing all failing. This suggests either:
- File system permissions issue in test environment
- Telemetry initialization failure
- File sink implementation bug affecting all operations

---

## Recommended Investigation Order

1. **Telemetry/File Sink (5 failures)** - Most failures, likely root cause affects multiple tests
2. **Tilde Expansion (3 failures)** - Core config functionality broken
3. **Dispatch/Activity (2 failures)** - Timeout and activity detection broken
4. **Git Path Filtering (1 failure)** - Isolated issue

## Next Steps

1. Investigate telemetry file sink initialization and file system permissions
2. Review tilde expansion implementation in config module
3. Check test environment setup for activity detection tests
4. Verify git command behavior in untracked file filtering

**Raw test output preserved in:** `test_full_output.log`
