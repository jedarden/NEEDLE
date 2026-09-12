//! Process-spawning and worker-lifecycle integration tests.
//!
//! This test target isolates tests that spawn real `needle` subprocesses or
//! manage complex worker lifecycle scenarios. These tests are separated from
//! the main integration_tests.rs target because:
//!
//! 1. **Process isolation**: Tests here spawn real subprocesses that may have
//!    different environment needs or cleanup requirements.
//! 2. **Parallel execution**: This target can be run independently with
//!    `cargo test --test integration_spawn` without blocking other tests.
//! 3. **Lifecycle focus**: Tests here specifically exercise worker startup,
//!    shutdown, signal handling, and process cleanup patterns.
//!
//! # Test Categories
//!
//! - Worker process spawning and termination
//! - Signal handling (SIGTERM, SIGINT, SIGHUP)
//! - Heartbeat file cleanup on abnormal exits
//! - Dead worker detection and orphan reaping
//! - Multi-worker coordination scenarios
//!
//! # Isolation Requirements
//!
//! All tests in this target MUST isolate both `$HOME` and any workspace scan
//! roots to prevent contamination of the real bead store. See
//! `docs/testing-isolation-patterns.md` for detailed patterns.

#[path = "integration_spawn/binary_freshness_fix_loop_e2e.rs"]
mod binary_freshness_fix_loop_e2e;
#[path = "integration_spawn/cleanup_function_error_handling_tests.rs"]
mod cleanup_function_error_handling_tests;
#[path = "integration_spawn/cleanup_liveness_regression.rs"]
mod cleanup_liveness_regression;
#[path = "integration_spawn/cli_integration.rs"]
mod cli_integration;
#[path = "integration_spawn/concurrent_startup_test.rs"]
mod concurrent_startup_test;
#[path = "integration_spawn/config_cli_tests.rs"]
mod config_cli_tests;
#[path = "integration_spawn/doctor_exit_code_tests.rs"]
mod doctor_exit_code_tests;
#[path = "integration_spawn/error_log_verification.rs"]
mod error_log_verification;
#[path = "integration_spawn/etxtbsy_retry.rs"]
mod etxtbsy_retry;
#[path = "integration_spawn/hard_timeout_tests.rs"]
mod hard_timeout_tests;
#[path = "integration_spawn/heartbeat_validation.rs"]
mod heartbeat_validation;
#[path = "integration_spawn/hot_reload_reexec.rs"]
mod hot_reload_reexec;
#[path = "integration_spawn/idle_timeout_tests.rs"]
mod idle_timeout_tests;
#[path = "integration_spawn/init_cli_tests.rs"]
mod init_cli_tests;
#[path = "integration_spawn/log_capture_helper.rs"]
mod log_capture_helper;
#[path = "integration_spawn/logs_stats_cli_tests.rs"]
mod logs_stats_cli_tests;
#[path = "integration_spawn/needle_transform_claude.rs"]
mod needle_transform_claude;
#[path = "integration_spawn/panic_safety_verification.rs"]
mod panic_safety_verification;
#[path = "integration_spawn/panic_stack_trace_capture.rs"]
mod panic_stack_trace_capture;
#[path = "integration_spawn/process_discovery_integration.rs"]
mod process_discovery_integration;
#[path = "integration_spawn/sigpipe_test.rs"]
mod sigpipe_test;
#[path = "integration_spawn/sigterm_heartbeat_cleanup.rs"]
mod sigterm_heartbeat_cleanup;
#[path = "integration_spawn/stop_kills_process_tree.rs"]
mod stop_kills_process_tree;
#[path = "integration_spawn/test_panic_safety_verification.rs"]
mod test_panic_safety_verification;
#[path = "integration_spawn/tmux_fixture.rs"]
mod tmux_fixture;
#[path = "integration_spawn/unbuffer_regression_test.rs"]
mod unbuffer_regression_test;
#[path = "integration_spawn/verify_bash_wrapper_exclusion.rs"]
mod verify_bash_wrapper_exclusion;
#[path = "integration_spawn/verify_deleted_binary_hot_reload.rs"]
mod verify_deleted_binary_hot_reload;
#[path = "integration_spawn/verify_process_discovery.rs"]
mod verify_process_discovery;
#[path = "integration_spawn/zcode_headless_adapter.rs"]
mod zcode_headless_adapter;
