# Tilde Expansion Config Fields Survey

**Generated:** 2026-08-28
**Bead:** needle-4beeff65
**Task:** Survey all config path fields requiring tilde expansion

## Overview

This document lists ALL configuration path fields in NEEDLE that undergo tilde expansion via the `Config::expand_tildes()` method. The expansion is performed using `expand_tilde()`, `expand_tilde_option()`, and `expand_tilde_vec()` helper functions defined in `src/config/mod.rs`.

## Complete Field List (18 total)

### 1. workspace.default
- **Type:** `PathBuf`
- **Section:** `workspace`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_global_config()`, `test_config_expand_tildes_mixed_paths()`, `test_config_expand_tildes_missing_home()`
- **Integration test coverage:** ⚠️ Partial - Used in integration tests but not specifically tested for tilde expansion
- **Test file:** `src/config/mod.rs:11552`

### 2. workspace.home
- **Type:** `PathBuf`
- **Section:** `workspace`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_global_config()`, `test_config_expand_tildes_mixed_paths()`, `test_config_expand_tildes_missing_home()`
- **Integration test coverage:** ⚠️ Partial - Heavily used in integration tests but not specifically tested for tilde expansion
- **Test file:** `src/config/mod.rs:11552`

### 3. worker.worker_binary_path
- **Type:** `Option<PathBuf>`
- **Section:** `worker`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_worker_binary_path()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11698`

### 4. agent.adapters_dir
- **Type:** `PathBuf`
- **Section:** `agent`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_global_config()`, `test_config_expand_tildes_mixed_paths()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11552`

### 5. bead_cli.path
- **Type:** `Option<PathBuf>`
- **Section:** `bead_cli`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_bead_cli_path()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11682`

### 6. strands.explore.workspace_root
- **Type:** `PathBuf`
- **Section:** `strands.explore`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_workspace_config()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11578`

### 7. strands.explore.workspaces
- **Type:** `Vec<PathBuf>`
- **Section:** `strands.explore`
- **Expand function:** `expand_tilde_vec()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_workspace_config()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11578`

### 8. strands.weave.exclude_workspaces
- **Type:** `Vec<PathBuf>`
- **Section:** `strands.weave`
- **Expand function:** `expand_tilde_vec()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_workspace_config()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11578`

### 9. strands.splice.report_workspace
- **Type:** `Option<PathBuf>`
- **Section:** `strands.splice`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_workspace_config()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11578`

### 10. post_push_ci.state_dir
- **Type:** `Option<PathBuf>`
- **Section:** `post_push_ci`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ❌ No
- **Integration test coverage:** ❌ No
- **Test file:** N/A

### 11. strands.learning.global_learnings_file
- **Type:** `PathBuf`
- **Section:** `strands.learning`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_env_var_paths()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11613`

### 12. telemetry.file_sink.log_dir
- **Type:** `Option<PathBuf>`
- **Section:** `telemetry.file_sink`
- **Expand function:** `expand_tilde()` (with if-let)
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_telemetry_log_dir()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11738`

### 13. health.heartbeat_dir
- **Type:** `Option<PathBuf>`
- **Section:** `health`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_env_var_paths()`
- **Integration test coverage:** ⚠️ Partial - Used in heartbeat tests but not specifically tested for tilde expansion
- **Test file:** `src/config/mod.rs:11613`

### 14. supervisor.heartbeat_path
- **Type:** `Option<PathBuf>`
- **Section:** `supervisor`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_env_var_paths()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11613`

### 15. supervisor.socket_path
- **Type:** `Option<PathBuf>`
- **Section:** `supervisor`
- **Expand function:** `expand_tilde_option()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_env_var_paths()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11613`

### 16. prompt.context_files
- **Type:** `Vec<PathBuf>`
- **Section:** `prompt`
- **Expand function:** `expand_tilde_vec()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_context_files_vector()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11714`

### 17. self_modification.canary_workspace
- **Type:** `PathBuf`
- **Section:** `self_modification`
- **Expand function:** `expand_tilde()`
- **Unit test coverage:** ✅ Yes - `test_config_expand_tildes_self_modification_canary_workspace()`
- **Integration test coverage:** ❌ No
- **Test file:** `src/config/mod.rs:11754`

### 18. prompt.variants[].content_file
- **Type:** `PathBuf`
- **Section:** `prompt.variants`
- **Expand function:** `expand_tilde()` (iterated in loop)
- **Unit test coverage:** ❌ No
- **Integration test coverage:** ❌ No
- **Test file:** N/A

## Summary

### Fields WITH unit test coverage (16/18 = 89%)
✅ All major fields have unit test coverage in `src/config/mod.rs`:
- Lines 11552-11942 contain comprehensive `test_config_expand_tildes_*` tests
- Coverage includes basic tilde expansion, missing HOME, mixed paths, vectors, options

### Fields MISSING unit test coverage (2/18 = 11%)
❌ **Need new unit tests:**
1. **post_push_ci.state_dir** - Option<PathBuf> field
2. **prompt.variants[].content_file** - PathBuf field in variant map

### Integration test coverage status
⚠️ **Limited:** Only `workspace.default`, `workspace.home`, and `health.heartbeat_dir` appear in integration tests, but none specifically test tilde expansion behavior in integration context.

### Recommendations

1. **Add unit tests for missing fields:**
   - `test_config_expand_tildes_post_push_ci_state_dir()` 
   - `test_config_expand_tildes_prompt_variants_content_file()`

2. **Consider integration test coverage:**
   - Integration tests should verify that tilde expansion works correctly when NEEDLE actually reads config from YAML files
   - Test that `~/path` in `.needle.yaml` or `.needle.d/config.yaml` expands correctly
   - Test with missing HOME environment variable in real worker execution

3. **Test isolation patterns:**
   - All tilde expansion tests use `crate::util::test_env::isolate_env()` for HOME isolation
   - Integration tests spawning NEEDLE subprocesses must set `cmd.env("HOME", temp_dir.path())` (see `docs/testing-isolation-patterns.md`)

## Helper Functions

The following helper functions in `src/config/mod.rs` perform tilde expansion:

- `expand_tilde(path: &Path) -> PathBuf` (line 8257)
- `expand_tilde_str(path: &str) -> String` (line 8308)
- `expand_tilde_vec(paths: &[PathBuf]) -> Vec<PathBuf>` (line 8336)
- `expand_tilde_option(path: &Option<PathBuf>) -> Option<PathBuf>` (line 8341)

## Related Documentation

- `docs/testing-isolation-patterns.md` - Comprehensive guide on test isolation for HOME/PATH
- Memory: `tilde-expansion-test-pattern.md` - Test isolation setup, config fields requiring coverage
- `src/util.rs` - Core `expand_tilde()`, `is_tilde_prefix()` functions
- `src/config/mod.rs` - `Config::expand_tildes()` method (line 6568)
