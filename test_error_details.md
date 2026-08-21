# Test Error Details

**Generated:** 2026-08-21
**Scopes:** `cargo test --lib` and `cargo test --test integration_tests`
**Capture mode:** `RUST_BACKTRACE=full`, with serial targeted reruns (`--test-threads=1`)

This report records every test failure observed in the current library and integration-test evidence. Each section is keyed by the exact test name and contains the complete panic/error block copied from its capture, including every backtrace frame. No included trace block contains an omitted-frame marker.

## Summary

| # | Scope | Test | Observed result | Error message | Panic location |
| ---: | --- | --- | --- | --- | --- |
| 1 | library | `config::config_tests::changed_sections_detects_multiple_section_changes` | failed (exit 101) | `should detect at least 3 changed sections` | `src/config/mod.rs:11818:9` |
| 2 | library | `config::config_tests::test_otlp_config_matches_plan_md` | failed (exit 101) | `plan.md config should load successfully: telemetry: unknown field `\`otlp\`\`` | `src/config/mod.rs:11560:9` |
| 3 | library | `strand::pluck::tests::sanitize_workspace_name_handles_various_paths` | failed (exit 101) | assertion `left == right` failed (`left: ""`, `right: "unknown"`) | `src/strand/pluck.rs:2461:9` |
| 4 | integration | `adapter_validation_rejects_special_characters` | failed (exit 101) | `error message should not execute injected payloads for adapter: '../../../etc/passwd'` | `tests/integration_tests.rs:2005:9` |
| 5 | integration | `ci_verification_test_failure` | failed (exit 101) | `CI VERIFICATION TEST FAILURE - This test is meant to fail to verify needle-ci retryStrategy fix surfaces test failures correctly` | `tests/integration_tests.rs:7027:5` |
| 6 | integration | `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` | failed (exit 101) | `br create failed:` | `tests/integration_tests.rs:2634:5` |
| 7 | integration | `cross_workspace_mend_skips_beads_with_live_assignees` | failed (exit 101) | `br create failed` | `tests/integration_tests.rs:2782:5` |
| 8 | integration | `cross_workspace_mend_skips_own_worker_beads` | failed (exit 101) | `br create failed` | `tests/integration_tests.rs:2904:5` |
| 9 | integration | `dead_worker_cleanup_integration` | failed (exit 101) | `needle worker failed with exit status: ExitStatus(unix_wait_status(512))` | `tests/integration_tests.rs:3379:5` |
| 10 | integration | `exhaustion_with_idle_action_wait_survives_sleep` | panic observed in full run; run timed out at 240s (exit 124) | assertion `left == right` failed: worker should process the bead that appeared after idle sleep (`left: 0`, `right: 1`) | `tests/integration_tests.rs:1095:5` |
| 11 | integration | `idle_worker_flagging_detects_stuck_workers` | failed (exit 101) | `configured bead-forge test store: bead backend binary not found at /home/coding/.local/bin/bf` | `tests/integration_tests.rs:199:10` |
| 12 | integration | `mend_removes_stale_dependency_links` | failed (exit 101) | `br dep add failed` | `tests/integration_tests.rs:3053:5` |
| 13 | integration | `subprocess_adapter_failure_exits_nonzero` | failed (exit 101) | `stderr should mention the nonexistent adapter; got: error: unrecognized subcommand 'worker'` | `tests/integration_tests.rs:6959:5` |
| 14 | integration | `worker_binary_path_supervisor_initialization` | failed (exit 101) | `supervisor should be created successfully with worker_binary_path: failed to initialize bead store for supervisor` (cause: workspace has no authoritative bead backend binding) | `tests/integration_tests.rs:3996:10` |

## Run-level notes

- Library suite command: `RUST_BACKTRACE=full timeout 300 cargo test --lib`. It emitted three `FAILED` markers, then exited 72 after the worker test process was externally terminated; all three failures were independently rerun and confirmed with exit 101.
- Integration suite command: `RUST_BACKTRACE=full timeout 240 cargo test --test integration_tests -- --test-threads=1 --nocapture`. It exited 124 while `exhaustion_with_idle_action_wait_survives_sleep` was running; that test’s panic block is preserved below.
- The other ten integration failures were independently rerun with `RUST_BACKTRACE=full cargo test --test integration_tests <name> -- --exact --nocapture --test-threads=1`; each exited 101 and produced a complete trace.

## Failure details

### 1. `config::config_tests::changed_sections_detects_multiple_section_changes`

```text
thread 'config::config_tests::changed_sections_detects_multiple_section_changes' (2565090) panicked at src/config/mod.rs:11818:9:
should detect at least 3 changed sections
stack backtrace:
   0:     0x5645369c2cda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x5645369c2cda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x5645369c2cda - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x5645369c2cda - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x5645369dc3ea - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x5645369dc3ea - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x5645369c9a92 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x5645369c9a92 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x56453699ad4f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x56453699ad4f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x5645369b7fd1 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x5645369b824b - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x56453699ae3a - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:691:13
  13:     0x564536991c89 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x56453699c19d - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x5645369dcd3c - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x564536022c8c - needle::config::config_tests::changed_sections_detects_multiple_section_changes::hd33cdf4b6d94be25
  17:     0x5645359d40c9 - needle::config::config_tests::changed_sections_detects_multiple_section_changes::{{closure}}::hb60fb72e2349170f
                               at /home/coding/NEEDLE/src/config/mod.rs:11807:59
  18:     0x5645359d40c9 - core::ops::function::FnOnce::call_once::h0e34388ec030c645
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  19:     0x5645364dad3b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  20:     0x5645364dad3b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  21:     0x5645364e772b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  22:     0x5645364e772b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  23:     0x5645364e772b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  24:     0x5645364e772b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  25:     0x5645364e772b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  26:     0x5645364e772b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  27:     0x5645364e772b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  28:     0x5645364e2e44 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  29:     0x5645364e2e44 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  30:     0x5645364ea332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  31:     0x5645364ea332 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  32:     0x5645364ea332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  33:     0x5645364ea332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  34:     0x5645364ea332 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  35:     0x5645364ea332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  36:     0x5645364ea332 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  37:     0x5645369c182f - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  38:     0x5645369c182f - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  39:     0x7fe0bb67db7b - <unknown>
  40:     0x7fe0bb6fb7f8 - <unknown>
  41:                0x0 - <unknown>
FAILED
```

### 2. `config::config_tests::test_otlp_config_matches_plan_md`

```text
thread 'config::config_tests::test_otlp_config_matches_plan_md' (2565109) panicked at src/config/mod.rs:11560:9:
plan.md config should load successfully: Some(Error("telemetry: unknown field `otlp`, expected one of `file_sink`, `stdout_sink`, `hooks`, `otlp_sink`", line: 3, column: 3))
stack backtrace:
   0:     0x5579582a0cda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x5579582a0cda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x5579582a0cda - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x5579582a0cda - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x5579582ba3ea - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x5579582ba3ea - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x5579582a7a92 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x5579582a7a92 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x557958278d4f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x557958278d4f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x557958295fd1 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55795829624b - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x557958278e08 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x55795826fc89 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55795827a19d - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x5579582bad3c - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x5579578dea82 - needle::config::config_tests::test_otlp_config_matches_plan_md::h49be429325309c01
                               at /home/coding/NEEDLE/src/config/mod.rs:11560:9
  17:     0x5579572c3289 - needle::config::config_tests::test_otlp_config_matches_plan_md::{{closure}}::hff92f6d572e6df69
                               at /home/coding/NEEDLE/src/config/mod.rs:11531:42
  18:     0x5579572c3289 - core::ops::function::FnOnce::call_once::h62bacbeeeaae4fc8
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  19:     0x557957db8d3b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  20:     0x557957db8d3b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  21:     0x557957dc572b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  22:     0x557957dc572b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  23:     0x557957dc572b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  24:     0x557957dc572b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  25:     0x557957dc572b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  26:     0x557957dc572b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  27:     0x557957dc572b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  28:     0x557957dc0e44 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  29:     0x557957dc0e44 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  30:     0x557957dc8332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  31:     0x557957dc8332 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  32:     0x557957dc8332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  33:     0x557957dc8332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  34:     0x557957dc8332 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  35:     0x557957dc8332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  36:     0x557957dc8332 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  37:     0x55795829f82f - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  38:     0x55795829f82f - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  39:     0x7f862ac1cb7b - <unknown>
  40:     0x7f862ac9a7f8 - <unknown>
  41:                0x0 - <unknown>
FAILED
```

### 3. `strand::pluck::tests::sanitize_workspace_name_handles_various_paths`

```text
thread 'strand::pluck::tests::sanitize_workspace_name_handles_various_paths' (2565128) panicked at src/strand/pluck.rs:2461:9:
assertion `left == right` failed
  left: ""
 right: "unknown"
stack backtrace:
   0:     0x557dbd2abcda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x557dbd2abcda - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x557dbd2abcda - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x557dbd2abcda - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x557dbd2c53ea - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x557dbd2c53ea - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x557dbd2b2a92 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x557dbd2b2a92 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x557dbd283d4f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x557dbd283d4f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x557dbd2a0fd1 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x557dbd2a124b - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x557dbd283e08 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x557dbd27ac89 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x557dbd28519d - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x557dbd2c5d3c - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x557dbd2c5bc3 - core[c1f1a4ba060b9bfa]::panicking::assert_failed_inner
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:439:17
  17:     0x557dbc77b788 - core::panicking::assert_failed::h729b3bad6d40949a
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:394:5
  18:     0x557dbc732c6a - needle::strand::pluck::tests::sanitize_workspace_name_handles_various_paths::ha67d7979b16a0bda
                               at /home/coding/NEEDLE/src/strand/pluck.rs:2461:9
  19:     0x557dbc2d5b69 - needle::strand::pluck::tests::sanitize_workspace_name_handles_various_paths::{{closure}}::h24b2bbe5bf8966e9
                               at /home/coding/NEEDLE/src/strand/pluck.rs:2445:55
  20:     0x557dbc2d5b69 - core::ops::function::FnOnce::call_once::h83453b1b10df8e27
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  21:     0x557dbcdc3d3b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  22:     0x557dbcdc3d3b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  23:     0x557dbcdd072b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  24:     0x557dbcdd072b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  25:     0x557dbcdd072b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  26:     0x557dbcdd072b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  27:     0x557dbcdd072b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  28:     0x557dbcdd072b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  29:     0x557dbcdd072b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  30:     0x557dbcdcbe44 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  31:     0x557dbcdcbe44 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  32:     0x557dbcdd3332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  33:     0x557dbcdd3332 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  34:     0x557dbcdd3332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  35:     0x557dbcdd3332 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  36:     0x557dbcdd3332 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  37:     0x557dbcdd3332 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  38:     0x557dbcdd3332 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  39:     0x557dbd2aa82f - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  40:     0x557dbd2aa82f - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  41:     0x7fb134a61b7b - <unknown>
  42:     0x7fb134adf7f8 - <unknown>
  43:                0x0 - <unknown>
FAILED
```

### 4. `adapter_validation_rejects_special_characters`

```text
thread 'adapter_validation_rejects_special_characters' (2476900) panicked at tests/integration_tests.rs:2005:9:
error message should not execute injected payloads for adapter: '../../../etc/passwd'
stack backtrace:
   0:     0x557400ed43ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x557400ed43ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x557400ed43ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x557400ed43ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x557400eedeaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x557400eedeaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x557400edb5f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x557400edb5f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x557400ead05f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x557400ead05f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x557400eca441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x557400eca6bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x557400ead118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x557400ea4029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x557400eae4ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x557400eee7fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55740043570b - integration_tests::adapter_validation_rejects_special_characters::{{closure}}::h1018d4b1962dd446
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2005:9
  17:     0x5574004cf520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x5574004cf520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x5574004cf520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x5574004cf520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x5574004cf520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x5574004cf520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x5574004cf520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x5574004e3d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x5574004e3d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x5574004e3d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x5574004cf704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x5574004cf704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x5574004cf704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x5574004cf704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x5574004cf704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x5574004cf704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x5574004ff2d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x5574004ff2d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x5574004cee15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x5574004cee15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x5574004cee15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55740044a96b - integration_tests::adapter_validation_rejects_special_characters::h2234d882f268a5c8
                               at /home/coding/NEEDLE/tests/integration_tests.rs:1985:50
  39:     0x55740044a96b - integration_tests::adapter_validation_rejects_special_characters::{{closure}}::hf0d414c09d765289
                               at /home/coding/NEEDLE/tests/integration_tests.rs:1971:57
  40:     0x55740044a96b - core::ops::function::FnOnce::call_once::h649163064f248a87
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55740050e31b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55740050e31b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55740051ad0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55740051ad0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55740051ad0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55740051ad0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55740051ad0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55740051ad0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55740051ad0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x557400516424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x557400516424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55740051d912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55740051d912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55740051d912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55740051d912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55740051d912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55740051d912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55740051d912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x557400ed2fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x557400ed2fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7f82bb157b7b - <unknown>
  62:     0x7f82bb1d57f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 5. `ci_verification_test_failure`

```text
thread 'ci_verification_test_failure' (2477355) panicked at tests/integration_tests.rs:7027:5:
CI VERIFICATION TEST FAILURE - This test is meant to fail to verify needle-ci retryStrategy fix surfaces test failures correctly
stack backtrace:
   0:     0x55b3471073ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55b3471073ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55b3471073ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55b3471073ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55b347120eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55b347120eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55b34710e5f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55b34710e5f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55b3470e005f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55b3470e005f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55b3470fd441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55b3470fd6bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55b3470e014a - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:691:13
  13:     0x55b3470d7029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55b3470e14ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55b3471217fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55b34663d902 - integration_tests::ci_verification_test_failure::{{closure}}::h3077501b213c4bd7
                               at /home/coding/NEEDLE/tests/integration_tests.rs:7027:5
  17:     0x55b346702520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x55b346702520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x55b346702520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x55b346702520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x55b346702520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x55b346702520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x55b346702520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x55b346716d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x55b346716d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x55b346716d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x55b346702704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x55b346702704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x55b346702704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x55b346702704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x55b346702704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x55b346702704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x55b3467322d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x55b3467322d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x55b346701e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x55b346701e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x55b346701e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55b34667ddae - integration_tests::ci_verification_test_failure::h0627cc798cb2ec65
                               at /home/coding/NEEDLE/tests/integration_tests.rs:7027:143
  39:     0x55b34667ddae - integration_tests::ci_verification_test_failure::{{closure}}::h72f96c3d3bb382f3
                               at /home/coding/NEEDLE/tests/integration_tests.rs:7025:40
  40:     0x55b34667ddae - core::ops::function::FnOnce::call_once::h691b53ba87a8d5f0
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55b34674131b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55b34674131b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55b34674dd0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55b34674dd0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55b34674dd0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55b34674dd0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55b34674dd0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55b34674dd0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55b34674dd0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x55b346749424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x55b346749424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55b346750912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55b346750912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55b346750912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55b346750912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55b346750912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55b346750912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55b346750912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x55b347105fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x55b347105fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7f9e01120b7b - <unknown>
  62:     0x7f9e0119e7f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 6. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`

```text
thread 'cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead' (2477394) panicked at tests/integration_tests.rs:2634:5:
br create failed: [one trailing space in original output]
stack backtrace:
   0:     0x55b6c13543ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55b6c13543ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55b6c13543ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55b6c13543ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55b6c136deaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55b6c136deaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55b6c135b5f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55b6c135b5f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55b6c132d05f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55b6c132d05f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55b6c134a441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55b6c134a6bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55b6c132d118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x55b6c1324029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55b6c132e4ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55b6c136e7fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55b6c08c6610 - integration_tests::cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead::{{closure}}::hacd3d450dd4ba0e0
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2634:5
  17:     0x55b6c094f520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x55b6c094f520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x55b6c094f520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x55b6c094f520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x55b6c094f520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x55b6c094f520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x55b6c094f520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x55b6c0963d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x55b6c0963d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x55b6c0963d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x55b6c094f704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x55b6c094f704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x55b6c094f704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x55b6c094f704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x55b6c094f704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x55b6c094f704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x55b6c097f2d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x55b6c097f2d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x55b6c094ee15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x55b6c094ee15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x55b6c094ee15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55b6c08c750e - integration_tests::cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead::h5b135e12c489f4d8
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2713:18
  39:     0x55b6c08c750e - integration_tests::cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead::{{closure}}::h5f09b149d1318b62
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2596:78
  40:     0x55b6c08c750e - core::ops::function::FnOnce::call_once::h0e6dba3226d142da
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55b6c098e31b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55b6c098e31b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55b6c099ad0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55b6c099ad0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55b6c099ad0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55b6c099ad0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55b6c099ad0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55b6c099ad0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55b6c099ad0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x55b6c0996424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x55b6c0996424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55b6c099d912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55b6c099d912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55b6c099d912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55b6c099d912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55b6c099d912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55b6c099d912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55b6c099d912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x55b6c1352fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x55b6c1352fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7ff398ae8b7b - <unknown>
  62:     0x7ff398b667f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 7. `cross_workspace_mend_skips_beads_with_live_assignees`

```text
thread 'cross_workspace_mend_skips_beads_with_live_assignees' (2477444) panicked at tests/integration_tests.rs:2782:5:
br create failed
stack backtrace:
   0:     0x55a11769e3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55a11769e3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55a11769e3ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55a11769e3ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55a1176b7eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55a1176b7eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55a1176a55f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55a1176a55f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55a11767705f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55a11767705f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55a117694441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55a1176946bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55a11767714a - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:691:13
  13:     0x55a11766e029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55a1176784ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55a1176b87fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55a116c088b3 - integration_tests::cross_workspace_mend_skips_beads_with_live_assignees::{{closure}}::hd317d731c8ac37a1
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2782:5
  17:     0x55a116c99520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x55a116c99520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x55a116c99520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x55a116c99520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x55a116c99520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x55a116c99520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x55a116c99520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x55a116cadd72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x55a116cadd72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x55a116cadd72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x55a116c99704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x55a116c99704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x55a116c99704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x55a116c99704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x55a116c99704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x55a116c99704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x55a116cc92d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x55a116cc92d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x55a116c98e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x55a116c98e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x55a116c98e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55a116c1cc3e - integration_tests::cross_workspace_mend_skips_beads_with_live_assignees::h6acce65eda385b5b
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2876:6
  39:     0x55a116c1cc3e - integration_tests::cross_workspace_mend_skips_beads_with_live_assignees::{{closure}}::hfa0a70211c1eb564
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2746:64
  40:     0x55a116c1cc3e - core::ops::function::FnOnce::call_once::hfec737f34e2a2073
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55a116cd831b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55a116cd831b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55a116ce4d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55a116ce4d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55a116ce4d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55a116ce4d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55a116ce4d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55a116ce4d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55a116ce4d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x55a116ce0424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x55a116ce0424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55a116ce7912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55a116ce7912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55a116ce7912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55a116ce7912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55a116ce7912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55a116ce7912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55a116ce7912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x55a11769cfcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x55a11769cfcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7fdfa43aeb7b - <unknown>
  62:     0x7fdfa442c7f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 8. `cross_workspace_mend_skips_own_worker_beads`

```text
thread 'cross_workspace_mend_skips_own_worker_beads' (2477470) panicked at tests/integration_tests.rs:2904:5:
br create failed
stack backtrace:
   0:     0x55adb1b023ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55adb1b023ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55adb1b023ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55adb1b023ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55adb1b1beaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55adb1b1beaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55adb1b095f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55adb1b095f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55adb1adb05f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55adb1adb05f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55adb1af8441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55adb1af86bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55adb1adb14a - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:691:13
  13:     0x55adb1ad2029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55adb1adc4ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55adb1b1c7fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55adb105feb3 - integration_tests::cross_workspace_mend_skips_own_worker_beads::{{closure}}::h4647fec603caa57b
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2904:5
  17:     0x55adb10fd520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x55adb10fd520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x55adb10fd520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x55adb10fd520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x55adb10fd520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x55adb10fd520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x55adb10fd520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x55adb1111d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x55adb1111d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x55adb1111d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x55adb10fd704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x55adb10fd704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x55adb10fd704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x55adb10fd704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x55adb10fd704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x55adb10fd704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x55adb112d2d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x55adb112d2d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x55adb10fce15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x55adb10fce15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x55adb10fce15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55adb107566e - integration_tests::cross_workspace_mend_skips_own_worker_beads::h825288367505e46c
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2984:6
  39:     0x55adb107566e - integration_tests::cross_workspace_mend_skips_own_worker_beads::{{closure}}::hedc289a3f850004e
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2880:55
  40:     0x55adb107566e - core::ops::function::FnOnce::call_once::h153bd184a662c95d
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55adb113c31b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55adb113c31b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55adb1148d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55adb1148d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55adb1148d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55adb1148d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55adb1148d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55adb1148d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55adb1148d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x55adb1144424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x55adb1144424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55adb114b912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55adb114b912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55adb114b912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55adb114b912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55adb114b912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55adb114b912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55adb114b912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x55adb1b00fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x55adb1b00fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7fdfc1d1bb7b - <unknown>
  62:     0x7fdfc1d997f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 9. `dead_worker_cleanup_integration`

```text
thread 'dead_worker_cleanup_integration' (2477498) panicked at tests/integration_tests.rs:3379:5:
needle worker failed with exit status: ExitStatus(unix_wait_status(512))
stack backtrace:
   0:     0x5624a9a223ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x5624a9a223ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x5624a9a223ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x5624a9a223ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x5624a9a3beaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x5624a9a3beaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x5624a9a295f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x5624a9a295f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x5624a99fb05f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x5624a99fb05f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x5624a9a18441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x5624a9a186bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x5624a99fb118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x5624a99f2029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x5624a99fc4ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x5624a9a3c7fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x5624a8f5e3fa - integration_tests::dead_worker_cleanup_integration::{{closure}}::h6d0b76080536feab
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3379:5
  17:     0x5624a901d520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x5624a901d520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x5624a901d520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x5624a901d520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x5624a901d520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x5624a901d520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x5624a901d520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x5624a9031d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x5624a9031d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x5624a9031d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x5624a901d704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x5624a901d704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x5624a901d704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x5624a901d704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x5624a901d704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x5624a901d704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x5624a904d2d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x5624a904d2d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x5624a901ce15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x5624a901ce15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x5624a901ce15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x5624a8f9e24e - integration_tests::dead_worker_cleanup_integration::h9a68958fb51c62c0
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3409:6
  39:     0x5624a8f9e24e - integration_tests::dead_worker_cleanup_integration::{{closure}}::hc93c6b44fa722523
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3285:43
  40:     0x5624a8f9e24e - core::ops::function::FnOnce::call_once::he95c1dc07619f9e5
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x5624a905c31b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x5624a905c31b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x5624a9068d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x5624a9068d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x5624a9068d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x5624a9068d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x5624a9068d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x5624a9068d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x5624a9068d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x5624a9064424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x5624a9064424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x5624a906b912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x5624a906b912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x5624a906b912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x5624a906b912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x5624a906b912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x5624a906b912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x5624a906b912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x5624a9a20fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x5624a9a20fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7f5ec7ec5b7b - <unknown>
  62:     0x7f5ec7f437f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 10. `exhaustion_with_idle_action_wait_survives_sleep`

```text
thread 'exhaustion_with_idle_action_wait_survives_sleep' (2493224) panicked at tests/integration_tests.rs:1095:5:
assertion `left == right` failed: worker should process the bead that appeared after idle sleep
  left: 0
 right: 1
stack backtrace:
   0:     0x56478bdde3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x56478bdde3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x56478bdde3ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x56478bdde3ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x56478bdf7eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x56478bdf7eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x56478bde55f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x56478bde55f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x56478bdb705f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x56478bdb705f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x56478bdd4441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x56478bdd46bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x56478bdb7118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x56478bdae029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x56478bdb84ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x56478bdf87fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x56478bdf86e3 - core[c1f1a4ba060b9bfa]::panicking::assert_failed_inner
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:434:23
  17:     0x56478bdf23f2 - core[c1f1a4ba060b9bfa]::panicking::assert_failed::<u64, u64>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:394:5
  18:     0x56478b3429b0 - integration_tests::exhaustion_with_idle_action_wait_survives_sleep::{{closure}}::h25bc13670fed0cc3
                               at /home/coding/NEEDLE/tests/integration_tests.rs:1095:5
  19:     0x56478b3d9520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  20:     0x56478b3d9520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  21:     0x56478b3d9520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  22:     0x56478b3d9520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  23:     0x56478b3d9520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  24:     0x56478b3d9520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  25:     0x56478b3d9520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  26:     0x56478b3edd72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  27:     0x56478b3edd72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  28:     0x56478b3edd72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  29:     0x56478b3d9704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  30:     0x56478b3d9704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  31:     0x56478b3d9704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  32:     0x56478b3d9704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  33:     0x56478b3d9704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  34:     0x56478b3d9704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  35:     0x56478b4092d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  36:     0x56478b4092d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  37:     0x56478b3d8e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  38:     0x56478b3d8e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  39:     0x56478b3d8e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  40:     0x56478b359f9b - integration_tests::exhaustion_with_idle_action_wait_survives_sleep::hd9bde1b0c4655e89
                               at /home/coding/NEEDLE/tests/integration_tests.rs:1099:6
  41:     0x56478b359f9b - integration_tests::exhaustion_with_idle_action_wait_survives_sleep::{{closure}}::h35192f3860370f47
                               at /home/coding/NEEDLE/tests/integration_tests.rs:874:59
  42:     0x56478b359f9b - core::ops::function::FnOnce::call_once::he77eb419e6a24502
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  43:     0x56478b41831b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  44:     0x56478b41831b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  45:     0x56478b424d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  46:     0x56478b424d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  47:     0x56478b424d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  48:     0x56478b424d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  49:     0x56478b424d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  50:     0x56478b424d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  51:     0x56478b424d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  52:     0x56478b420424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  53:     0x56478b420424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  54:     0x56478b427912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  55:     0x56478b427912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  56:     0x56478b427912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  57:     0x56478b427912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  58:     0x56478b427912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  59:     0x56478b427912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  60:     0x56478b427912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  61:     0x56478bddcfcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  62:     0x56478bddcfcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  63:     0x7f03efd06b7b - <unknown>
  64:     0x7f03efd847f8 - <unknown>
  65:                0x0 - <unknown>
FAILED
```

### 11. `idle_worker_flagging_detects_stuck_workers`

```text
thread 'idle_worker_flagging_detects_stuck_workers' (2477516) panicked at tests/integration_tests.rs:199:10:
configured bead-forge test store: bead backend binary not found at /home/coding/.local/bin/bf

Stack backtrace:
   0: anyhow::error::<impl anyhow::Error>::msg
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/anyhow-1.0.104/src/backtrace.rs:10:14
   1: needle::bead_store::cli_store::CliBeadStore::new
             at ./src/bead_store/cli_store.rs:46:13
   2: integration_tests::configured_forge_store
             at ./tests/integration_tests.rs:198:5
   3: integration_tests::idle_worker_flagging_detects_stuck_workers::{{closure}}
             at ./tests/integration_tests.rs:3242:17
   4: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
   5: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
   6: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
   7: tokio::task::coop::with_budget
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
   8: tokio::task::coop::budget
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
   9: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  10: tokio::runtime::scheduler::current_thread::Context::enter
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  11: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  12: tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  13: tokio::runtime::context::scoped::Scoped<T>::set
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  14: tokio::runtime::context::set_scheduler::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  15: std::thread::local::LocalKey<T>::try_with
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  16: std::thread::local::LocalKey<T>::with
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  17: tokio::runtime::context::set_scheduler
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  18: tokio::runtime::scheduler::current_thread::CoreGuard::enter
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  19: tokio::runtime::scheduler::current_thread::CoreGuard::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  20: tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  21: tokio::runtime::context::runtime::enter_runtime
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  22: tokio::runtime::scheduler::current_thread::CurrentThread::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  23: tokio::runtime::runtime::Runtime::block_on_inner
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  24: tokio::runtime::runtime::Runtime::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  25: integration_tests::idle_worker_flagging_detects_stuck_workers
             at ./tests/integration_tests.rs:3281:6
  26: integration_tests::idle_worker_flagging_detects_stuck_workers::{{closure}}
             at ./tests/integration_tests.rs:3167:54
  27: core::ops::function::FnOnce::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  28: <fn() -> core::result::Result<(), alloc::string::String> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  29: test::__rust_begin_short_backtrace::<core::result::Result<(), alloc::string::String>, fn() -> core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  30: test::run_test_in_process::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  31: <core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  32: std::panicking::catch_unwind::do_call::<core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>, core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  33: std::panicking::catch_unwind::<core::result::Result<(), alloc::string::String>, core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  34: std::panic::catch_unwind::<core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>, core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  35: test::run_test_in_process
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  36: test::run_test::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  37: test::run_test::{closure#1}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  38: std::sys::backtrace::__rust_begin_short_backtrace::<test::run_test::{closure#1}, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  39: std::thread::lifecycle::spawn_unchecked::<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  40: <core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  41: std::panicking::catch_unwind::do_call::<core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  42: std::panicking::catch_unwind::<(), core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  43: std::panic::catch_unwind::<core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  44: std::thread::lifecycle::spawn_unchecked::<test::run_test::{closure#1}, ()>::{closure#1}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  45: <std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  46: <alloc::boxed::Box<dyn core::ops::function::FnOnce<(), Output = ()> + core::marker::Send> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  47: <std::sys::thread::unix::Thread>::new::thread_start
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  48: <unknown>
  49: <unknown>
stack backtrace:
   0:     0x55ed328a83ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55ed328a83ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55ed328a83ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55ed328a83ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55ed328c1eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55ed328c1eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55ed328af5f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55ed328af5f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55ed3288105f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55ed3288105f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55ed3289e441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55ed3289e6bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55ed32881118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x55ed32878029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55ed328824ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55ed328c27fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55ed328c2542 - core[c1f1a4ba060b9bfa]::result::unwrap_failed
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/result.rs:1867:5
  17:     0x55ed31ddc799 - core::result::Result<T,E>::expect::h69d3dd122b9f9a6a
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/result.rs:1185:23
  18:     0x55ed31ddc799 - integration_tests::configured_forge_store::h282765378d0fc687
                               at /home/coding/NEEDLE/tests/integration_tests.rs:199:10
  19:     0x55ed31e02118 - integration_tests::idle_worker_flagging_detects_stuck_workers::{{closure}}::h967d664ae0289452
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3242:17
  20:     0x55ed31ea3520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  21:     0x55ed31ea3520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  22:     0x55ed31ea3520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  23:     0x55ed31ea3520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  24:     0x55ed31ea3520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  25:     0x55ed31ea3520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  26:     0x55ed31ea3520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  27:     0x55ed31eb7d72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  28:     0x55ed31eb7d72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  29:     0x55ed31eb7d72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  30:     0x55ed31ea3704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  31:     0x55ed31ea3704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  32:     0x55ed31ea3704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  33:     0x55ed31ea3704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  34:     0x55ed31ea3704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  35:     0x55ed31ea3704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  36:     0x55ed31ed32d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  37:     0x55ed31ed32d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  38:     0x55ed31ea2e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  39:     0x55ed31ea2e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  40:     0x55ed31ea2e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  41:     0x55ed31e1b25e - integration_tests::idle_worker_flagging_detects_stuck_workers::h19d5527b831547bf
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3281:6
  42:     0x55ed31e1b25e - integration_tests::idle_worker_flagging_detects_stuck_workers::{{closure}}::h862d0837c1ca4f31
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3167:54
  43:     0x55ed31e1b25e - core::ops::function::FnOnce::call_once::h030a06cfe6b743bc
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  44:     0x55ed31ee231b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  45:     0x55ed31ee231b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  46:     0x55ed31eeed0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  47:     0x55ed31eeed0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  48:     0x55ed31eeed0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  49:     0x55ed31eeed0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  50:     0x55ed31eeed0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  51:     0x55ed31eeed0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  52:     0x55ed31eeed0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  53:     0x55ed31eea424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  54:     0x55ed31eea424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  55:     0x55ed31ef1912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  56:     0x55ed31ef1912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  57:     0x55ed31ef1912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  58:     0x55ed31ef1912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  59:     0x55ed31ef1912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  60:     0x55ed31ef1912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  61:     0x55ed31ef1912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  62:     0x55ed328a6fcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  63:     0x55ed328a6fcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  64:     0x7f3b63668b7b - <unknown>
  65:     0x7f3b636e67f8 - <unknown>
  66:                0x0 - <unknown>
FAILED
```

### 12. `mend_removes_stale_dependency_links`

```text
thread 'mend_removes_stale_dependency_links' (2477536) panicked at tests/integration_tests.rs:3053:5:
br dep add failed
stack backtrace:
   0:     0x5599bf33d3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x5599bf33d3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x5599bf33d3ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x5599bf33d3ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x5599bf356eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x5599bf356eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x5599bf3445f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x5599bf3445f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x5599bf31605f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x5599bf31605f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x5599bf333441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x5599bf3336bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x5599bf31614a - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:691:13
  13:     0x5599bf30d029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x5599bf3174ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x5599bf3577fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x5599be886a75 - integration_tests::mend_removes_stale_dependency_links::{{closure}}::h615ab471ef8a23bd
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3053:5
  17:     0x5599be938520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x5599be938520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x5599be938520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x5599be938520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x5599be938520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x5599be938520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x5599be938520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x5599be94cd72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x5599be94cd72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x5599be94cd72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x5599be938704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x5599be938704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x5599be938704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x5599be938704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x5599be938704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x5599be938704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x5599be9682d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x5599be9682d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x5599be937e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x5599be937e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x5599be937e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x5599be8b478e - integration_tests::mend_removes_stale_dependency_links::hd980186d20e8c8d9
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3159:6
  39:     0x5599be8b478e - integration_tests::mend_removes_stale_dependency_links::{{closure}}::h6f3bd3ad3860e455
                               at /home/coding/NEEDLE/tests/integration_tests.rs:2992:47
  40:     0x5599be8b478e - core::ops::function::FnOnce::call_once::h7d50faf3b922f0d2
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x5599be97731b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x5599be97731b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x5599be983d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x5599be983d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x5599be983d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x5599be983d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x5599be983d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x5599be983d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x5599be983d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x5599be97f424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x5599be97f424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x5599be986912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x5599be986912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x5599be986912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x5599be986912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x5599be986912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x5599be986912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x5599be986912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x5599bf33bfcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x5599bf33bfcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7f46e09e3b7b - <unknown>
  62:     0x7f46e0a617f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 13. `subprocess_adapter_failure_exits_nonzero`

```text
thread 'subprocess_adapter_failure_exits_nonzero' (2477560) panicked at tests/integration_tests.rs:6959:5:
stderr should mention the nonexistent adapter; got: error: unrecognized subcommand 'worker'

Usage: needle <COMMAND>

For more information, try '--help'.

stack backtrace:
   0:     0x55cd4705d3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55cd4705d3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55cd4705d3ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55cd4705d3ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55cd47076eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55cd47076eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55cd470645f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55cd470645f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55cd4703605f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55cd4703605f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55cd47053441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55cd470536bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55cd47036118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x55cd4702d029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55cd470374ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55cd470777fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55cd465b0d20 - integration_tests::subprocess_adapter_failure_exits_nonzero::{{closure}}::hc003c849f6f31893
                               at /home/coding/NEEDLE/tests/integration_tests.rs:6959:5
  17:     0x55cd46658520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  18:     0x55cd46658520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  19:     0x55cd46658520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  20:     0x55cd46658520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  21:     0x55cd46658520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  22:     0x55cd46658520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  23:     0x55cd46658520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  24:     0x55cd4666cd72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  25:     0x55cd4666cd72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  26:     0x55cd4666cd72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  27:     0x55cd46658704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  28:     0x55cd46658704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  29:     0x55cd46658704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  30:     0x55cd46658704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  31:     0x55cd46658704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  32:     0x55cd46658704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  33:     0x55cd466882d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  34:     0x55cd466882d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  35:     0x55cd46657e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  36:     0x55cd46657e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  37:     0x55cd46657e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  38:     0x55cd465d0bbe - integration_tests::subprocess_adapter_failure_exits_nonzero::hf5616396ee36c30c
                               at /home/coding/NEEDLE/tests/integration_tests.rs:6965:6
  39:     0x55cd465d0bbe - integration_tests::subprocess_adapter_failure_exits_nonzero::{{closure}}::hc7b3586f8e837ec8
                               at /home/coding/NEEDLE/tests/integration_tests.rs:6840:52
  40:     0x55cd465d0bbe - core::ops::function::FnOnce::call_once::h1c81b5e3281a8a48
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  41:     0x55cd4669731b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  42:     0x55cd4669731b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  43:     0x55cd466a3d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  44:     0x55cd466a3d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  45:     0x55cd466a3d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  46:     0x55cd466a3d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  47:     0x55cd466a3d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  48:     0x55cd466a3d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  49:     0x55cd466a3d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  50:     0x55cd4669f424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  51:     0x55cd4669f424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  52:     0x55cd466a6912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  53:     0x55cd466a6912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  54:     0x55cd466a6912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  55:     0x55cd466a6912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  56:     0x55cd466a6912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  57:     0x55cd466a6912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  58:     0x55cd466a6912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  59:     0x55cd4705bfcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  60:     0x55cd4705bfcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  61:     0x7f744e643b7b - <unknown>
  62:     0x7f744e6c17f8 - <unknown>
  63:                0x0 - <unknown>
FAILED
```

### 14. `worker_binary_path_supervisor_initialization`

```text
thread 'worker_binary_path_supervisor_initialization' (2477581) panicked at tests/integration_tests.rs:3996:10:
supervisor should be created successfully with worker_binary_path: failed to initialize bead store for supervisor

Caused by:
    workspace /home/coding/.tmp/.tmpNMrzgn/workspace has no authoritative bead backend binding; set bead_cli.backend in /home/coding/.tmp/.tmpNMrzgn/workspace/.needle.yaml

Stack backtrace:
   0: anyhow::error::<impl anyhow::Error>::msg
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/anyhow-1.0.104/src/backtrace.rs:10:14
   1: needle::bead_store::open_configured
             at ./src/bead_store/mod.rs:52:9
   2: needle::bead_store::discover_default
             at ./src/bead_store/mod.rs:336:5
   3: needle::supervisor::Supervisor::new
             at ./src/supervisor/mod.rs:412:41
   4: integration_tests::worker_binary_path_supervisor_initialization::{{closure}}
             at ./tests/integration_tests.rs:3995:23
   5: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
   6: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
   7: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
   8: tokio::task::coop::with_budget
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
   9: tokio::task::coop::budget
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  10: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  11: tokio::runtime::scheduler::current_thread::Context::enter
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  12: tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  13: tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  14: tokio::runtime::context::scoped::Scoped<T>::set
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  15: tokio::runtime::context::set_scheduler::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  16: std::thread::local::LocalKey<T>::try_with
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  17: std::thread::local::LocalKey<T>::with
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  18: tokio::runtime::context::set_scheduler
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  19: tokio::runtime::scheduler::current_thread::CoreGuard::enter
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  20: tokio::runtime::scheduler::current_thread::CoreGuard::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  21: tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  22: tokio::runtime::context::runtime::enter_runtime
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  23: tokio::runtime::scheduler::current_thread::CurrentThread::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  24: tokio::runtime::runtime::Runtime::block_on_inner
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  25: tokio::runtime::runtime::Runtime::block_on
             at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  26: integration_tests::worker_binary_path_supervisor_initialization
             at ./tests/integration_tests.rs:3999:63
  27: integration_tests::worker_binary_path_supervisor_initialization::{{closure}}
             at ./tests/integration_tests.rs:3963:56
  28: core::ops::function::FnOnce::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  29: <fn() -> core::result::Result<(), alloc::string::String> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  30: test::__rust_begin_short_backtrace::<core::result::Result<(), alloc::string::String>, fn() -> core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  31: test::run_test_in_process::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  32: <core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  33: std::panicking::catch_unwind::do_call::<core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>, core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  34: std::panicking::catch_unwind::<core::result::Result<(), alloc::string::String>, core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  35: std::panic::catch_unwind::<core::panic::unwind_safe::AssertUnwindSafe<test::run_test_in_process::{closure#0}>, core::result::Result<(), alloc::string::String>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  36: test::run_test_in_process
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  37: test::run_test::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  38: test::run_test::{closure#1}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  39: std::sys::backtrace::__rust_begin_short_backtrace::<test::run_test::{closure#1}, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  40: std::thread::lifecycle::spawn_unchecked::<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  41: <core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  42: std::panicking::catch_unwind::do_call::<core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  43: std::panicking::catch_unwind::<(), core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  44: std::panic::catch_unwind::<core::panic::unwind_safe::AssertUnwindSafe<std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  45: std::thread::lifecycle::spawn_unchecked::<test::run_test::{closure#1}, ()>::{closure#1}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  46: <std::thread::lifecycle::spawn_unchecked<test::run_test::{closure#1}, ()>::{closure#1} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  47: <alloc::boxed::Box<dyn core::ops::function::FnOnce<(), Output = ()> + core::marker::Send> as core::ops::function::FnOnce<()>>::call_once
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  48: <std::sys::thread::unix::Thread>::new::thread_start
             at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  49: <unknown>
  50: <unknown>
stack backtrace:
   0:     0x55667adde3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::libunwind::trace
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1:     0x55667adde3ca - std[e28293b1aa0f68bd]::backtrace_rs::backtrace::trace_unsynchronized::<std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt::{closure#1}>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
   2:     0x55667adde3ca - std[e28293b1aa0f68bd]::sys::backtrace::_print_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:74:9
   3:     0x55667adde3ca - <<std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print::DisplayBacktrace as core[c1f1a4ba060b9bfa]::fmt::Display>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:44:26
   4:     0x55667adf7eaa - <core[c1f1a4ba060b9bfa]::fmt::rt::Argument>::fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/fmt/rt.rs:152:76
   5:     0x55667adf7eaa - core[c1f1a4ba060b9bfa]::fmt::write
   6:     0x55667ade55f2 - std[e28293b1aa0f68bd]::io::default_write_fmt::<std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:639:11
   7:     0x55667ade55f2 - <std[e28293b1aa0f68bd]::sys::stdio::unix::Stderr as std[e28293b1aa0f68bd]::io::Write>::write_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/io/mod.rs:1994:13
   8:     0x55667adb705f - <std[e28293b1aa0f68bd]::sys::backtrace::BacktraceLock>::print
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:47:9
   9:     0x55667adb705f - std[e28293b1aa0f68bd]::panicking::default_hook::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:292:27
  10:     0x55667add4441 - std[e28293b1aa0f68bd]::panicking::default_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:319:9
  11:     0x55667add46bb - std[e28293b1aa0f68bd]::panicking::panic_with_hook
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:825:13
  12:     0x55667adb7118 - std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:698:13
  13:     0x55667adae029 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_end_short_backtrace::<std[e28293b1aa0f68bd]::panicking::panic_handler::{closure#0}, !>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:182:18
  14:     0x55667adb84ad - __rustc[b7974e8690430dd9]::rust_begin_unwind
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:689:5
  15:     0x55667adf87fc - core[c1f1a4ba060b9bfa]::panicking::panic_fmt
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panicking.rs:80:14
  16:     0x55667adf8542 - core[c1f1a4ba060b9bfa]::result::unwrap_failed
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/result.rs:1867:5
  17:     0x55667a33ead3 - core::result::Result<T,E>::expect::haaf2cf52371a3f17
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/result.rs:1185:23
  18:     0x55667a33ead3 - integration_tests::worker_binary_path_supervisor_initialization::{{closure}}::hd23afe6127f0f70a
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3996:10
  19:     0x55667a3d9520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h87daff02fddb8e00
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  20:     0x55667a3d9520 - <core::pin::Pin<P> as core::future::future::Future>::poll::h1ff1419f95ab7252
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/future/future.rs:133:9
  21:     0x55667a3d9520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::{{closure}}::hf83099d8052f8924
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:70
  22:     0x55667a3d9520 - tokio::task::coop::with_budget::h17223078e470b2c9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:167:5
  23:     0x55667a3d9520 - tokio::task::coop::budget::hd9b9954d7f99e1e8
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/task/coop/mod.rs:133:5
  24:     0x55667a3d9520 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::{{closure}}::h94015129d853070a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:830:25
  25:     0x55667a3d9520 - tokio::runtime::scheduler::current_thread::Context::enter::hd86d2d88e0b1c0a2
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:488:19
  26:     0x55667a3edd72 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::{{closure}}::hd5705e84dc4e7beb
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:829:44
  27:     0x55667a3edd72 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::{{closure}}::hd17beca0862f678c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:68
  28:     0x55667a3edd72 - tokio::runtime::context::scoped::Scoped<T>::set::h0f6c987a0c3d887e
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/scoped.rs:40:9
  29:     0x55667a3d9704 - tokio::runtime::context::set_scheduler::{{closure}}::h16e534cb4ebccc1a
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:38
  30:     0x55667a3d9704 - std::thread::local::LocalKey<T>::try_with::he86964d640d4cd86
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:513:12
  31:     0x55667a3d9704 - std::thread::local::LocalKey<T>::with::h24440b6082844a43
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/local.rs:477:20
  32:     0x55667a3d9704 - tokio::runtime::context::set_scheduler::hddd7e7af9b02e40c
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context.rs:187:17
  33:     0x55667a3d9704 - tokio::runtime::scheduler::current_thread::CoreGuard::enter::hc2531c22132780b9
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:906:27
  34:     0x55667a3d9704 - tokio::runtime::scheduler::current_thread::CoreGuard::block_on::h251d25b8aebce5a6
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:817:24
  35:     0x55667a4092d3 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::{{closure}}::hf131befc28836e04
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:218:33
  36:     0x55667a4092d3 - tokio::runtime::context::runtime::enter_runtime::h4263ce3c1d352432
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/context/runtime.rs:65:16
  37:     0x55667a3d8e15 - tokio::runtime::scheduler::current_thread::CurrentThread::block_on::h390945614a35f6ee
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:206:9
  38:     0x55667a3d8e15 - tokio::runtime::runtime::Runtime::block_on_inner::hd84c04b8483b82fe
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:374:52
  39:     0x55667a3d8e15 - tokio::runtime::runtime::Runtime::block_on::h2925f86bc58c1973
                               at /home/coding/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.53.1/src/runtime/runtime.rs:343:18
  40:     0x55667a3562fe - integration_tests::worker_binary_path_supervisor_initialization::h08904209c02ab47c
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3999:63
  41:     0x55667a3562fe - integration_tests::worker_binary_path_supervisor_initialization::{{closure}}::h23e816b58865e862
                               at /home/coding/NEEDLE/tests/integration_tests.rs:3963:56
  42:     0x55667a3562fe - core::ops::function::FnOnce::call_once::ha31157e8cd85d916
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  43:     0x55667a41831b - <fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  44:     0x55667a41831b - test[273d7611820c9051]::__rust_begin_short_backtrace::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, fn() -> core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:663:18
  45:     0x55667a424d0b - test[273d7611820c9051]::run_test_in_process::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:74
  46:     0x55667a424d0b - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  47:     0x55667a424d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  48:     0x55667a424d0b - std[e28293b1aa0f68bd]::panicking::catch_unwind::<core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>, core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  49:     0x55667a424d0b - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<test[273d7611820c9051]::run_test_in_process::{closure#0}>, core[c1f1a4ba060b9bfa]::result::Result<(), alloc[fdfd2bd8633a6659]::string::String>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  50:     0x55667a424d0b - test[273d7611820c9051]::run_test_in_process
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:686:27
  51:     0x55667a424d0b - test[273d7611820c9051]::run_test::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:607:43
  52:     0x55667a420424 - test[273d7611820c9051]::run_test::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/test/src/lib.rs:637:41
  53:     0x55667a420424 - std[e28293b1aa0f68bd]::sys::backtrace::__rust_begin_short_backtrace::<test[273d7611820c9051]::run_test::{closure#1}, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/backtrace.rs:166:18
  54:     0x55667a427912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:91:13
  55:     0x55667a427912 - <core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/panic/unwind_safe.rs:274:9
  56:     0x55667a427912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::do_call::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:581:40
  57:     0x55667a427912 - std[e28293b1aa0f68bd]::panicking::catch_unwind::<(), core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panicking.rs:544:19
  58:     0x55667a427912 - std[e28293b1aa0f68bd]::panic::catch_unwind::<core[c1f1a4ba060b9bfa]::panic::unwind_safe::AssertUnwindSafe<std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}::{closure#0}>, ()>
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/panic.rs:359:14
  59:     0x55667a427912 - std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked::<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/thread/lifecycle.rs:89:26
  60:     0x55667a427912 - <std[e28293b1aa0f68bd]::thread::lifecycle::spawn_unchecked<test[273d7611820c9051]::run_test::{closure#1}, ()>::{closure#1} as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/core/src/ops/function.rs:250:5
  61:     0x55667addcfcf - <alloc[fdfd2bd8633a6659]::boxed::Box<dyn core[c1f1a4ba060b9bfa]::ops::function::FnOnce<(), Output = ()> + core[c1f1a4ba060b9bfa]::marker::Send> as core[c1f1a4ba060b9bfa]::ops::function::FnOnce<()>>::call_once
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/alloc/src/boxed.rs:2240:9
  62:     0x55667addcfcf - <std[e28293b1aa0f68bd]::sys::thread::unix::Thread>::new::thread_start
                               at /rustc/59807616e1fa2540724bfbac14d7976d7e4a3860/library/std/src/sys/thread/unix.rs:118:17
  63:     0x7f679c30db7b - <unknown>
  64:     0x7f679c38b7f8 - <unknown>
  65:                0x0 - <unknown>
FAILED
```

## Capture verification

- Confirmed failure sections: 14.
- Complete backtrace blocks: 14/14.
- Omitted-frame markers in trace blocks: 0.
- Every section is organized under its exact test name.
