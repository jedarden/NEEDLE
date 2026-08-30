# Process-Spawning Test Catalog

Generated: 2026-08-29
**Total Command::new sites in --lib tests: 25**

## Summary

| Category | Count |
|----------|-------|
| Worker-lifecycle | 1 |
| Process-spawning | 24 |
| Other | 0 |

## Detailed Catalog

The following 25 `Command::new` call sites were found in `#[cfg(test)]` modules across the codebase.


### Worker-lifecycle (1 entries)


#### src/cli/mod.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 6248 | `is_needle_inner_false_by_default` | Spawns NEEDLE/worker binary: &exe_path | `tokio::process::Command::new(&exe_path)` |

### Process-spawning (24 entries)


#### src/bead_store/cli_store.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 1146 | `run_bf_batch_retries_mock_etxtbsy_then_succeeds` | External tool: true | `tokio::process::Command::new("true").spawn()` |

#### src/bead_store/mod.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 2849 | `verify_backend_identity_spawn_site_retries_on_etxtbsy` | External tool: echo | `std::process::Command::new("echo")` |
| 2963 | `verify_backend_identity_spawn_site_succeeds_on_first_attempt` | External tool: echo | `std::process::Command::new("echo")` |

#### src/ci.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 1555 | `commit_correlation_requires_one_unambiguous_trailer` | External tool: git | `std::process::Command::new("git")` |

#### src/commit_hook.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 510 | `empty_head_means_no_op` | External tool: git | `let output = Command::new("git")` |
| 787 | `line_787` | External tool: git | `let output = Command::new("git")` |
| 905 | `run_git_in_dir` | External tool: git | `let output = Command::new("git")` |

#### src/mitosis/timeout_context.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 677 | `git_dirty_paths_filters_untracked` | External tool: git | `tokio::process::Command::new("git")` |
| 693 | `line_693` | External tool: git | `tokio::process::Command::new("git")` |
| 705 | `line_705` | External tool: git | `tokio::process::Command::new("git")` |
| 743 | `git_dirty_paths_captures_modified` | External tool: git | `tokio::process::Command::new("git")` |
| 759 | `line_759` | External tool: git | `tokio::process::Command::new("git")` |
| 769 | `line_769` | External tool: git | `tokio::process::Command::new("git")` |

#### src/registry/mod.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 771 | `is_pid_alive_returns_false_for_nonexistent_pid` | External tool: true | `let child = std::process::Command::new("true")` |

#### src/scratch_sweep.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 835 | `git` | External tool: git | `let output = Command::new("git")` |
| 850 | `git` | External tool: git | `let output = Command::new("git")` |

#### src/supervisor/mod.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 1876 | `reap_zombie_children_reaps_an_exited_child` | External tool: true | `let child = std::process::Command::new("true")` |

#### src/util.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 2259 | `set_hermetic_probe_path` | External tool: which | `let which_dir = std::process::Command::new("which")` |

#### src/validation/mod.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 1614 | `git_init` | External tool: git | `std::process::Command::new("git")` |
| 1620 | `git_init` | External tool: git | `std::process::Command::new("git")` |
| 1626 | `git_init` | External tool: git | `std::process::Command::new("git")` |
| 1634 | `git_add` | External tool: git | `std::process::Command::new("git")` |
| 1642 | `git_add` | External tool: git | `std::process::Command::new("git")` |

#### src/validation/shipped_work.rs

| Line | Test Function | Description | Code Snippet |
|------|---------------|-------------|--------------|
| 289 | `snapshot` | External tool: git | `std::process::Command::new("git")` |
