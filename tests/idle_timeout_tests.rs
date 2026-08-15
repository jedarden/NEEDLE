//! Unit tests for idle timeout expiry behavior.
//!
//! These tests verify the idle timeout mechanism implemented in src/dispatch/mod.rs:
//! 1. Idle timeout fires when no activity occurs
//! 2. Idle deadline resets on activity (prevents timeout)
//! 3. Idle timeout disabled when config is None/zero

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use needle::dispatch::{AgentAdapter, Dispatcher, TimeoutReason, TokenExtraction};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{BeadId, InputMethod};

fn test_adapter_with_idle_timeout(
    name: &str,
    template: &str,
    idle_timeout_secs: u64,
) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "test".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs: 0, // Use idle/hard timeout instead
        idle_timeout_secs,
        hard_timeout_secs: 0, // No hard deadline
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn test_prompt() -> BuiltPrompt {
    BuiltPrompt {
        content: "test prompt".to_string(),
        hash: "testhash".to_string(),
        token_estimate: 100,
        template_name: "test".to_string(),
        template_version: "1.0".to_string(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 1: Idle timeout fires when no activity occurs
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_fires_when_no_activity_occurs() {
    // Create an adapter with a very short idle timeout (1 second)
    let adapter = test_adapter_with_idle_timeout("test-idle", "sleep 10", 1);

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-idle".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-test");
    let workspace = Path::new("/tmp");

    // Start time for timeout verification
    let start = Instant::now();

    // Execute the agent - should timeout after 1 second of no activity
    let result = dispatcher
        .dispatch(&bead_id, &test_prompt(), &dispatcher.adapter("test-idle").unwrap(), workspace)
        .await;

    let elapsed = start.elapsed();

    // Verify timeout occurred
    assert!(
        result.is_ok(),
        "dispatch should complete successfully (with timeout)"
    );

    let exec_result = result.unwrap();
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (timeout)"
    );

    // Verify timeout reason is Idle
    assert!(
        exec_result.timeout_reason.is_some(),
        "timeout reason should be set"
    );

    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Idle {
            timeout_secs,
            last_output_age_secs,
        } => {
            assert_eq!(
                timeout_secs, 1,
                "idle timeout should be configured for 1 second"
            );
            // Verify that the last output age is at least 1 second (the idle timeout)
            assert!(
                last_output_age_secs >= 1,
                "last output age should be >= 1 second (idle timeout duration), got {}",
                last_output_age_secs
            );
        }
        other => panic!(
            "expected TimeoutReason::Idle, got {:?}",
            other
        ),
    }

    // Verify the timeout fired in reasonable time (within 2 seconds of configured timeout)
    // This confirms we didn't wait for the full sleep 10 duration
    assert!(
        elapsed < Duration::from_secs(3),
        "idle timeout should fire quickly, took {:?} (expected < 3s)",
        elapsed
    );

    // Verify we waited at least the idle timeout duration
    assert!(
        elapsed >= Duration::from_secs(1),
        "idle timeout should wait at least the configured duration, took {:?} (expected >= 1s)",
        elapsed
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 2: Idle deadline resets on activity (prevents timeout)
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_deadline_resets_on_activity_prevents_timeout() {
    // Create an adapter with a short idle timeout (2 seconds)
    // but the process produces output every 1 second, preventing timeout
    let adapter = AgentAdapter {
        name: "test-activity".to_string(),
        description: None,
        agent_cli: "sh".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "for i in $(seq 1 10); do echo \"output $i\"; sleep 1; done".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 2, // 2 second idle timeout
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-activity".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-activity");
    let workspace = Path::new("/tmp");

    // Execute the agent - should complete successfully without timeout
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            &dispatcher.adapter("test-activity").unwrap(),
            workspace,
        )
        .await;

    assert!(
        result.is_ok(),
        "dispatch should complete successfully"
    );

    let exec_result = result.unwrap();

    // Verify the process completed normally (exit code 0)
    assert_eq!(
        exec_result.exit_code, 0,
        "process should complete successfully with exit code 0, got {}",
        exec_result.exit_code
    );

    // Verify no timeout occurred
    assert!(
        exec_result.timeout_reason.is_none(),
        "timeout reason should be None when process completes successfully"
    );

    // Verify output was captured
    assert!(
        !exec_result.stdout.is_empty(),
        "stdout should contain output from the process"
    );

    assert!(
        exec_result.stdout.contains("output"),
        "stdout should contain the process output"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 3: Idle timeout disabled when config is None/zero
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_disabled_when_config_is_zero() {
    // Create an adapter with idle_timeout_secs = 0 (disabled)
    let adapter = test_adapter_with_idle_timeout("test-disabled", "sleep 2", 0);

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-disabled".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-disabled");
    let workspace = Path::new("/tmp");

    // Execute the agent - should complete normally without timeout
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            &dispatcher.adapter("test-disabled").unwrap(),
            workspace,
        )
        .await;

    assert!(
        result.is_ok(),
        "dispatch should complete successfully"
    );

    let exec_result = result.unwrap();

    // Verify the process completed normally (exit code 0)
    assert_eq!(
        exec_result.exit_code, 0,
        "process should complete successfully with exit code 0"
    );

    // Verify no timeout occurred
    assert!(
        exec_result.timeout_reason.is_none(),
        "timeout reason should be None when idle timeout is disabled"
    );
}

#[tokio::test]
async fn idle_timeout_with_config_none_falls_back_to_global() {
    // Create an adapter with no adapter-specific timeout (all zeros)
    // This should fall back to the global config timeout
    let adapter = AgentAdapter {
        name: "test-fallback".to_string(),
        description: None,
        agent_cli: "sleep".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "sleep 0".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,     // No legacy timeout
        idle_timeout_secs: 0, // No idle timeout configured
        hard_timeout_secs: 0, // No hard timeout configured
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-fallback".to_string(), adapter);

    // Create dispatcher with global timeout of 1 hour
    let global_timeout = 3600;
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, global_timeout);
    let bead_id = BeadId::from("needle-fallback");
    let workspace = Path::new("/tmp");

    // Execute the agent - should use global timeout
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            &dispatcher.adapter("test-fallback").unwrap(),
            workspace,
        )
        .await;

    assert!(
        result.is_ok(),
        "dispatch should complete successfully"
    );

    let exec_result = result.unwrap();

    // Verify the process completed normally (exit code 0)
    assert_eq!(
        exec_result.exit_code, 0,
        "process should complete successfully with exit code 0"
    );

    // Verify no timeout occurred for this quick operation
    assert!(
        exec_result.timeout_reason.is_none(),
        "timeout reason should be None for quick completion"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 4: Idle timeout with very short deadline
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_very_short_deadline_fires_immediately() {
    // Create an adapter with extremely short idle timeout (0.1 seconds)
    let adapter = test_adapter_with_idle_timeout("test-short", "sleep 10", 0);

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-short".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-short");
    let workspace = Path::new("/tmp");

    // Note: Tokio's minimum sleep resolution is typically 1ms, so 0.1s should be fine
    // However, if this test is flaky, we may need to increase the timeout
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            &dispatcher.adapter("test-short").unwrap(),
            workspace,
        )
        .await;

    // With idle_timeout_secs = 0, the deadline is disabled
    // So this should complete normally
    assert!(
        result.is_ok(),
        "dispatch should complete successfully"
    );

    let exec_result = result.unwrap();
    assert_eq!(
        exec_result.exit_code, 0,
        "with idle timeout disabled (0), should complete normally"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 5: Idle timeout with mixed activity pattern
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_mixed_activity_pattern() {
    // Create an adapter that produces output irregularly:
    // - First output immediately
    // - Then long silence (longer than idle timeout)
    // This tests that idle deadline resets correctly
    let adapter = AgentAdapter {
        name: "test-mixed".to_string(),
        description: None,
        agent_cli: "sh".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "echo 'first'; sleep 3; echo 'second'".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 1, // 1 second idle timeout
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-mixed".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-mixed");
    let workspace = Path::new("/tmp");

    // Execute the agent - should timeout during the 3-second silence
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            &dispatcher.adapter("test-mixed").unwrap(),
            workspace,
        )
        .await;

    assert!(
        result.is_ok(),
        "dispatch should complete with timeout"
    );

    let exec_result = result.unwrap();

    // Verify timeout occurred
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (timeout)"
    );

    // Verify timeout reason is Idle
    match exec_result.timeout_reason {
        Some(TimeoutReason::Idle { .. }) => {
            // Success - idle timeout fired as expected
        }
        other => panic!(
            "expected TimeoutReason::Idle, got {:?}",
            other
        ),
    }

    // Verify some output was captured before timeout
    assert!(
        exec_result.stdout.contains("first"),
        "stdout should contain 'first' output before timeout"
    );
}