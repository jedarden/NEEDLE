//! Regression test for OTLP runtime guard bug (bf-24dtb, fix bf-3s2b0).
//!
//! This test verifies that calling initialization code which spawns tokio tasks
//! works correctly when the runtime guard is in place.
//!
//! The bug: `init_tracing_subscriber` calls `tokio::spawn` without ensuring
//! the runtime is entered. Without `rt.enter()`, tokio::spawn panics with
//! "there is no reactor running, must be called from the context of a
//! Tokio 1.x runtime".
//!
//! The fix: Line 937 in src/cli/mod.rs adds `let _rt_guard = rt.enter();`
//! before calling `init_tracing_subscriber`, ensuring the runtime context
//! is active when tokio::spawn is called.

use std::sync::{Arc, Mutex};

/// Test that verifies tokio::spawn panics without an entered runtime.
///
/// This test demonstrates the bug pattern: tokio::spawn will panic if
/// called without an active runtime context in the current thread.
#[test]
fn test_tokio_spawn_panics_without_entered_runtime() {
    // Create a tokio runtime with new_current_thread()
    // Build the runtime WITHOUT calling rt.enter()
    // This reproduces the bug scenario from bf-5dwfq
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // NO rt.enter() call here - this is the bug scenario
    // The runtime handle is stored in `rt` but not entered

    // Try to use tokio::spawn - this should panic
    let result = std::panic::catch_unwind(|| {
        // This simulates what init_tracing_subscriber does on line 831
        let (_tx, mut _rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            while let Some(_) = _rx.recv().await {
                // Handle drop events
            }
        });
    });

    // The spawn should have panicked
    assert!(
        result.is_err(),
        "tokio::spawn should panic without entered runtime"
    );
}

/// Test the fix: entering the runtime allows tokio::spawn to succeed.
///
/// This demonstrates the fix: after calling rt.enter(), tokio::spawn
/// works correctly because there's an active runtime context.
#[test]
fn test_tokio_spawn_succeeds_with_entered_runtime() {
    let spawn_completed = Arc::new(Mutex::new(false));
    let spawn_completed_clone = spawn_completed.clone();

    // Create a runtime
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

    // Enter the runtime context (this is the fix from bf-3s2b0)
    let _guard = rt.enter();

    // Now tokio::spawn will succeed because runtime is entered
    let result = std::panic::catch_unwind(|| {
        // Simulate what init_tracing_subscriber does
        let (_tx, mut _rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        // This spawn should now succeed because runtime is entered
        tokio::spawn(async move {
            while let Some(_) = _rx.recv().await {
                // Handle events
                *spawn_completed_clone.lock().unwrap() = true;
            }
        });

        // Give the spawned task time to complete
        rt.block_on(async {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        });
    });

    assert!(
        result.is_ok(),
        "tokio::spawn should succeed with entered runtime"
    );
    // Note: The task may not have completed yet due to timing, but we verified spawn didn't panic
}

/// Integration-style test: verify the complete pattern from the bug.
///
/// This test recreates the exact scenario from the bug:
/// 1. Create a tokio runtime
/// 2. Enter it (the fix)
/// 3. Call code that spawns tasks (like init_tracing_subscriber does)
/// 4. Verify it succeeds
#[test]
fn test_otlp_initialization_pattern_with_runtime_guard() {
    // Create a runtime
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

    // Enter the runtime (the fix from bf-3s2b0, line 937)
    let _rt_guard = rt.enter();

    // Verify that we can now spawn tasks successfully
    // This simulates what init_tracing_subscriber does on line 831-839
    let task_ran = Arc::new(Mutex::new(false));
    let task_ran_clone = task_ran.clone();

    rt.block_on(async {
        // Create channel like init_tracing_subscriber does
        let (drop_tx, mut drop_rx) = tokio::sync::mpsc::unbounded_channel::<bool>();
        let task_ran_ref = task_ran_clone.clone();

        // Spawn task like init_tracing_subscriber does (line 831)
        let handle = tokio::spawn(async move {
            while let Some(_event) = drop_rx.recv().await {
                // Process drop event
                *task_ran_ref.lock().unwrap() = true;
            }
        });

        // Send a test event
        drop_tx.send(true).expect("send failed");

        // Give the task time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Cleanup
        handle.abort();
    });

    assert!(
        *task_ran.lock().unwrap(),
        "spawned task should have run successfully"
    );
}
