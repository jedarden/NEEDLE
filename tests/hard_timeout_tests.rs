//! Unit tests for hard timeout (absolute deadline) behavior.
//!
//! These tests verify the hard timeout mechanism implemented in src/dispatch/mod.rs:
//! 1. Hard timeout fires after absolute time regardless of activity
//! 2. Hard deadline never resets (unlike idle timeout)
//! 3. Hard timeout disabled when config is None/zero
//! 4. Hard timeout works independently of idle timeout

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use needle::dispatch::{AgentAdapter, Dispatcher, TimeoutReason, TokenExtraction};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{BeadId, InputMethod};

fn test_adapter_with_hard_timeout(
    name: &str,
    template: &str,
    hard_timeout_secs: u64,
) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "test".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,      // Use idle/hard timeout instead
        idle_timeout_secs: 0, // No idle deadline
        hard_timeout_secs,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn test_adapter_with_both_timeouts(
    name: &str,
    template: &str,
    idle_timeout_secs: u64,
    hard_timeout_secs: u64,
) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "test".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs,
        hard_timeout_secs,
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
// Test 1: Hard timeout fires after absolute time regardless of activity
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_timeout_fires_regardless_of_activity() {
    // Create an adapter with a 2 second hard timeout
    // The process produces output every 0.5 seconds (frequent activity)
    // but should still be killed at the 2 second hard deadline
    let adapter = AgentAdapter {
        name: "test-hard-activity".to_string(),
        description: None,
        agent_cli: "sh".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "for i in $(seq 1 20); do echo \"output $i\"; sleep 0.5; done".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 0, // No idle deadline
        hard_timeout_secs: 2, // 2 second hard deadline
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-activity".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-activity");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    // Execute the agent - should hard timeout after 2 seconds despite activity
    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-activity").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "dispatch should complete successfully (with hard timeout)"
    );

    let exec_result = result.unwrap();
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (timeout)"
    );

    // Verify timeout reason is Hard (not Idle)
    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Hard { timeout_secs } => {
            assert_eq!(
                timeout_secs, 2,
                "hard timeout should be configured for 2 seconds"
            );
        }
        other => panic!("expected TimeoutReason::Hard, got {:?}", other),
    }

    // Verify the hard timeout fired at approximately 2 seconds
    // (even though activity was occurring every 0.5 seconds)
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(4),
        "hard timeout should fire at ~2 seconds regardless of activity, took {:?}",
        elapsed
    );

    // Verify some output was captured before hard timeout
    assert!(
        !exec_result.stdout.is_empty(),
        "stdout should contain output before hard timeout"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 2: Hard deadline never resets (unlike idle timeout)
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_deadline_never_resets_on_activity() {
    // Create an adapter with a 1 second hard timeout
    // The process starts producing output after 0.5 seconds
    // But the hard deadline was set at t=0, so it should fire at t=1
    let adapter = AgentAdapter {
        name: "test-hard-noreseat".to_string(),
        description: None,
        agent_cli: "sh".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        // Sleep first, then produce output (tests that deadline doesn't reset)
        invoke_template: "sleep 0.5; echo 'late output'; sleep 10".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 0,
        hard_timeout_secs: 1, // 1 second hard deadline
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-noreseat".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-noreseat");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-noreseat").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Should hard timeout at ~1 second (not reset by the late output at 0.5s)
    assert_eq!(
        exec_result.exit_code, 124,
        "should hard timeout at 1 second despite activity at 0.5s"
    );

    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Hard { timeout_secs } => {
            assert_eq!(timeout_secs, 1);
        }
        other => panic!("expected TimeoutReason::Hard, got {:?}", other),
    }

    // Verify we didn't wait the full 10 seconds
    assert!(
        elapsed < Duration::from_secs(3),
        "hard timeout should fire at ~1 second, took {:?}",
        elapsed
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 3: Hard timeout disabled when config is None/zero
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_timeout_disabled_when_config_is_zero() {
    // Create an adapter with hard_timeout_secs = 0 (disabled)
    let adapter = test_adapter_with_hard_timeout("test-hard-disabled", "sleep 2", 0);

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-disabled".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-disabled");
    let workspace = Path::new("/tmp");

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-disabled").unwrap(),
            workspace,
        )
        .await;

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Verify the process completed normally (exit code 0 for sleep 2)
    assert_eq!(
        exec_result.exit_code, 0,
        "process should complete normally with exit code 0"
    );

    // Verify no timeout occurred
    assert!(
        exec_result.timeout_reason.is_none(),
        "timeout reason should be None when hard timeout is disabled"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 4: Hard timeout shorter than idle timeout
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_timeout_shorter_than_idle_timeout_fires_first() {
    // Create an adapter with:
    // - idle_timeout_secs: 10 seconds (resets on activity)
    // - hard_timeout_secs: 2 seconds (never resets)
    // Process produces output every 1 second (prevents idle timeout)
    // Hard timeout should fire at 2 seconds
    let adapter = test_adapter_with_both_timeouts(
        "test-hard-shorter",
        "for i in $(seq 1 20); do echo \"output $i\"; sleep 1; done",
        10, // 10 second idle timeout
        2,  // 2 second hard timeout (shorter)
    );

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-shorter".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-shorter");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-shorter").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Hard timeout should fire (not idle timeout)
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (timeout)"
    );

    // Verify timeout reason is Hard (not Idle)
    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Hard { timeout_secs } => {
            assert_eq!(
                timeout_secs, 2,
                "hard timeout should be the configured 2 seconds"
            );
        }
        other => panic!("expected TimeoutReason::Hard, got {:?}", other),
    }

    // Verify hard timeout fired at ~2 seconds (not idle timeout at 10)
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(4),
        "hard timeout should fire at ~2 seconds, took {:?}",
        elapsed
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 5: Idle timeout shorter than hard timeout
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_shorter_than_hard_timeout_fires_first() {
    // Create an adapter with:
    // - idle_timeout_secs: 1 second (resets on activity)
    // - hard_timeout_secs: 10 seconds (never resets)
    // Process has no activity after start
    // Idle timeout should fire at 1 second
    let adapter = test_adapter_with_both_timeouts(
        "test-idle-shorter",
        "sleep 20", // No output, will trigger idle timeout
        1,          // 1 second idle timeout (shorter)
        10,         // 10 second hard timeout
    );

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-idle-shorter".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-idle-shorter");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-idle-shorter").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Idle timeout should fire (not hard timeout)
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (timeout)"
    );

    // Verify timeout reason is Idle (not Hard)
    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Idle {
            timeout_secs,
            last_output_age_secs,
        } => {
            assert_eq!(
                timeout_secs, 1,
                "idle timeout should be the configured 1 second"
            );
            assert!(
                last_output_age_secs >= 1,
                "last output age should be >= 1 second"
            );
        }
        other => panic!("expected TimeoutReason::Idle, got {:?}", other),
    }

    // Verify idle timeout fired at ~1 second (not hard timeout at 10)
    assert!(
        elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3),
        "idle timeout should fire at ~1 second, took {:?}",
        elapsed
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 6: Hard timeout with very short deadline
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_timeout_very_short_deadline_fires_immediately() {
    // Create an adapter with a 0.5 second hard timeout
    // Process tries to produce output at 1 second
    // Hard timeout should fire at 0.5 seconds before any output
    let adapter = AgentAdapter {
        name: "test-hard-very-short".to_string(),
        description: None,
        agent_cli: "sh".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "sleep 1; echo 'too late'".to_string(),
        environment: HashMap::new(),
        timeout_secs: 0,
        idle_timeout_secs: 0,
        hard_timeout_secs: 1, // 1 second hard timeout
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    };

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-very-short".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-very-short");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-very-short").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Hard timeout should fire
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (hard timeout)"
    );

    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Hard { timeout_secs } => {
            assert_eq!(timeout_secs, 1);
        }
        other => panic!("expected TimeoutReason::Hard, got {:?}", other),
    }

    // Verify hard timeout fired at ~1 second
    assert!(
        elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3),
        "hard timeout should fire at ~1 second, took {:?}",
        elapsed
    );

    // Verify no output was captured (hard timeout fired before first output)
    assert!(
        !exec_result.stdout.contains("too late"),
        "stdout should not contain 'too late' (hard timeout fired first)"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 7: Hard timeout with long-running silent process
// ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hard_timeout_with_silent_process() {
    // Create an adapter with a 3 second hard timeout
    // Process runs for 20 seconds with no output
    // Hard timeout should fire at 3 seconds
    let adapter = test_adapter_with_hard_timeout("test-hard-silent", "sleep 20", 3);

    let telemetry = Telemetry::new("test-worker".to_string());
    let mut adapters = HashMap::new();
    adapters.insert("test-hard-silent".to_string(), adapter);

    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let bead_id = BeadId::from("needle-hard-silent");
    let workspace = Path::new("/tmp");

    let start = Instant::now();

    let result = dispatcher
        .dispatch(
            &bead_id,
            &test_prompt(),
            dispatcher.adapter("test-hard-silent").unwrap(),
            workspace,
        )
        .await;

    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dispatch should complete successfully");

    let exec_result = result.unwrap();

    // Hard timeout should fire
    assert_eq!(
        exec_result.exit_code, 124,
        "exit code should be 124 (hard timeout)"
    );

    match exec_result.timeout_reason.unwrap() {
        TimeoutReason::Hard { timeout_secs } => {
            assert_eq!(timeout_secs, 3);
        }
        other => panic!("expected TimeoutReason::Hard, got {:?}", other),
    }

    // Verify hard timeout fired at ~3 seconds (not the full 20)
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(6),
        "hard timeout should fire at ~3 seconds, took {:?}",
        elapsed
    );
}
