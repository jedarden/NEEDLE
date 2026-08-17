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

#[cfg(test)]
mod tests {
    // Stub module - tests will be added here
    // This target is enabled by default and can run independently
}
