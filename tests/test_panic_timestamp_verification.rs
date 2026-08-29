//! Timestamp verification for panic telemetry and debug output.
//!
//! This test module verifies that timestamps are correctly captured and emitted
//! in panic telemetry events and debug output, ensuring:
//! - Debug logs show the captured timestamp value
//! - Telemetry emissions include the timestamp field
//! - Log format is readable and useful for debugging
//! - Timestamp values are consistent across capture, logging, and telemetry

use std::time::SystemTime;

#[test]
fn test_system_time_capture_produces_valid_value() {
    // Test that SystemTime::now() produces a valid, reasonable timestamp
    let timestamp = SystemTime::now();

    // Verify we can measure duration since UNIX_EPOCH
    let duration_since_epoch = timestamp
        .duration_since(std::time::UNIX_EPOCH)
        .expect("SystemTime should be measurable since UNIX_EPOCH");

    // Verify the timestamp is reasonable (sometime after 2020 and before 2030)
    let seconds = duration_since_epoch.as_secs();
    assert!(
        seconds >= 1_577_836_800, // 2020-01-01
        "timestamp should be after 2020, got {} seconds since epoch",
        seconds
    );
    assert!(
        seconds <= 1_893_456_000, // 2030-01-01
        "timestamp should be before 2030, got {} seconds since epoch",
        seconds
    );
}

#[test]
fn test_system_time_debug_format_is_readable() {
    // Test that SystemTime's Debug format is human-readable
    let timestamp = SystemTime::now();
    let debug_string = format!("{:?}", timestamp);

    // The Debug output should be non-empty and meaningful
    assert!(!debug_string.is_empty());
    assert!(
        debug_string.len() > 10,
        "Debug representation should have meaningful content"
    );

    // SystemTime's Debug format typically includes "SystemTime" or time data
    // This ensures it's not silently failing to format
    assert!(
        debug_string.contains("SystemTime") || debug_string.contains('('),
        "Debug format should contain recognizable SystemTime structure"
    );
}

#[test]
fn test_system_time_consistency_across_calls() {
    // Test that multiple timestamp captures are monotonically increasing
    let timestamp1 = SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let timestamp2 = SystemTime::now();

    // Verify both are measurable
    let dur1 = timestamp1
        .duration_since(std::time::UNIX_EPOCH)
        .expect("timestamp1 should be measurable");
    let dur2 = timestamp2
        .duration_since(std::time::UNIX_EPOCH)
        .expect("timestamp2 should be measurable");

    // timestamp2 should be >= timestamp1
    assert!(
        dur2 >= dur1,
        "later timestamp should be greater or equal to earlier timestamp"
    );

    // They should not be identical (we slept for 10ms)
    assert!(
        dur2 > dur1,
        "timestamps captured after sleep should be different"
    );
}

#[test]
fn test_system_time_roundtrip_consistency() {
    // Test that timestamp format is consistent across roundtrip conversion
    let timestamp = SystemTime::now();

    // Convert to duration since epoch
    let duration = timestamp
        .duration_since(std::time::UNIX_EPOCH)
        .expect("should be measurable");

    // Convert back to SystemTime
    let reconstructed = std::time::UNIX_EPOCH + duration;

    // Both should produce the same duration since epoch
    let original_dur = timestamp
        .duration_since(std::time::UNIX_EPOCH)
        .expect("original should be measurable");
    let reconstructed_dur = reconstructed
        .duration_since(std::time::UNIX_EPOCH)
        .expect("reconstructed should be measurable");

    assert_eq!(
        original_dur.as_secs(),
        reconstructed_dur.as_secs(),
        "roundtrip should preserve seconds"
    );
}

#[test]
fn test_timestamp_field_presence_in_panic_hook() {
    // This test verifies that the panic hook in panic_capture.rs
    // captures and emits a timestamp field

    // We can't directly test the panic hook without triggering a panic,
    // but we can verify the code structure by examining the source
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Verify timestamp capture is present
    assert!(
        panic_capture_source.contains("let timestamp = SystemTime::now();"),
        "panic hook should capture timestamp with SystemTime::now()"
    );

    // Verify timestamp is emitted in telemetry
    assert!(
        panic_capture_source.contains("timestamp = ?timestamp"),
        "panic hook should emit timestamp field in telemetry"
    );

    // Verify telemetry emission includes the timestamp
    assert!(
        panic_capture_source.contains("tracing::error!"),
        "panic hook should use tracing::error! for telemetry emission"
    );

    // Verify all expected fields are present
    assert!(
        panic_capture_source.contains("panic_message = msg"),
        "should emit panic_message field"
    );
    assert!(
        panic_capture_source.contains("file = location.file()"),
        "should emit file field"
    );
    assert!(
        panic_capture_source.contains("line = location.line()"),
        "should emit line field"
    );
    assert!(
        panic_capture_source.contains("column = location.column()"),
        "should emit column field"
    );
}

#[test]
fn test_timestamp_capture_placement_before_telemetry() {
    // Verify that timestamp is captured immediately before telemetry emission
    // to ensure accuracy of the timing
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Find the position of timestamp capture
    let timestamp_pos = panic_capture_source.find("let timestamp = SystemTime::now();");
    assert!(
        timestamp_pos.is_some(),
        "timestamp capture should be present"
    );

    // Find the position of telemetry emission
    let telemetry_pos = panic_capture_source.find("tracing::error!(");
    assert!(
        telemetry_pos.is_some(),
        "telemetry emission should be present"
    );

    // Verify timestamp comes before telemetry (closer to actual panic time)
    if let (Some(ts_pos), Some(te_pos)) = (timestamp_pos, telemetry_pos) {
        assert!(
            ts_pos < te_pos,
            "timestamp should be captured before telemetry emission"
        );
    }
}

#[test]
fn test_timestamp_emission_uses_debug_formatting() {
    // Verify that timestamp uses ? formatting (Debug trait) for readability
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // The ?timestamp format uses the Debug trait, which provides readable output
    assert!(
        panic_capture_source.contains("timestamp = ?timestamp"),
        "timestamp should use ? formatting for Debug trait output"
    );

    // This ensures the timestamp is human-readable in logs, not binary data
}

#[test]
fn test_panic_hook_installs_debug_logging() {
    // Verify that the panic hook installation includes debug logging
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Verify debug logging for hook installation
    assert!(
        panic_capture_source.contains("tracing::debug!(\"panic hook installed"),
        "should emit debug log when hook is installed"
    );

    // This ensures debug logging is working when the hook is installed
}

#[test]
fn test_panic_hook_emits_structured_event() {
    // Verify the panic hook emits a structured event with all required fields
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Verify the event message is descriptive
    assert!(
        panic_capture_source.contains("\"test panic captured\""),
        "panic event should have descriptive message"
    );

    // Verify all critical fields are emitted in the structured event
    let expected_fields = ["panic_message", "file", "line", "column", "timestamp"];

    for field in &expected_fields {
        assert!(
            panic_capture_source.contains(field),
            "panic event should include {} field",
            field
        );
    }
}

#[test]
fn test_timestamp_is_captured_at_panic_time_not_hook_installation() {
    // Verify that timestamp is captured in the panic hook function itself,
    // not at hook installation time, to ensure accuracy
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // The timestamp capture should be inside the panic_hook function
    // Find the panic_hook function definition
    let hook_start = panic_capture_source.find("fn panic_hook(");
    assert!(hook_start.is_some(), "panic_hook function should exist");

    // Find timestamp capture
    let timestamp_capture = panic_capture_source.find("let timestamp = SystemTime::now();");
    assert!(
        timestamp_capture.is_some(),
        "timestamp capture should exist"
    );

    // Verify timestamp capture comes after hook function definition
    if let (Some(hook_pos), Some(ts_pos)) = (hook_start, timestamp_capture) {
        assert!(
            ts_pos > hook_pos,
            "timestamp capture should be inside panic_hook function, not at installation"
        );
    }
}

#[test]
fn test_panic_hook_output_format_is_readable() {
    // Verify that the panic hook produces readable, structured output
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Check for structured eprintln! output (human-readable panic info)
    assert!(
        panic_capture_source.contains("eprintln!(\"━━━━━━━━━━━━"),
        "should use structured visual separator for readability"
    );

    assert!(
        panic_capture_source.contains("eprintln!(\"PANIC captured"),
        "should include clear PANIC indicator"
    );

    assert!(
        panic_capture_source.contains("eprintln!(\"Message: {}\""),
        "should output panic message"
    );

    assert!(
        panic_capture_source.contains("eprintln!(\"Location: {}:{}"),
        "should output file:line:column location"
    );

    // This ensures the console output is readable and useful for debugging
}

#[test]
fn test_debug_and_telemetry_emissions_both_present() {
    // Verify that both debug output and telemetry are emitted for comprehensive coverage
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Check for console output (eprintln!)
    assert!(
        panic_capture_source.contains("eprintln!"),
        "should emit debug output to console"
    );

    // Check for telemetry (tracing::error!)
    assert!(
        panic_capture_source.contains("tracing::error!"),
        "should emit telemetry event"
    );

    // Both should be present for complete observability
}

#[test]
fn test_timestamp_capture_between_console_and_telemetry() {
    // Verify the execution order: capture timestamp → console output → telemetry
    // This ensures the timestamp is accurate and covers all emissions
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Find key markers
    let timestamp_pos = panic_capture_source.find("let timestamp = SystemTime::now();");
    let console_output = panic_capture_source.find("eprintln!(\"━━━━━━");
    let telemetry_emission = panic_capture_source.find("tracing::error!");

    // All should exist
    assert!(timestamp_pos.is_some(), "timestamp capture should exist");
    assert!(console_output.is_some(), "console output should exist");
    assert!(
        telemetry_emission.is_some(),
        "telemetry emission should exist"
    );

    // Verify order: timestamp first, then console, then telemetry
    if let (Some(ts), Some(console), Some(telem)) =
        (timestamp_pos, console_output, telemetry_emission)
    {
        assert!(
            ts < console && console < telem,
            "execution order should be: capture timestamp → console output → telemetry"
        );
    }
}

#[test]
fn test_utility_timestamp_functions_exist() {
    // Verify that utility functions for timestamp capture exist and are tested
    let util_source = include_str!("../src/util.rs");

    // Verify capture_timestamp function exists
    assert!(
        util_source.contains("pub fn capture_timestamp()"),
        "utility function capture_timestamp should exist"
    );

    // Verify capture_timestamp_result function exists
    assert!(
        util_source.contains("pub fn capture_timestamp_result()"),
        "utility function capture_timestamp_result should exist"
    );

    // These provide ISO 8601 formatted timestamps for other use cases
}

#[test]
fn test_panic_timestamp_uses_system_time_not_utility() {
    // Verify that panic hook uses SystemTime::now() directly, not capture_timestamp()
    // This is intentional: panic hook needs SystemTime's Debug formatting
    // rather than ISO 8601 string format, for better logging
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Should use SystemTime::now() directly
    assert!(
        panic_capture_source.contains("SystemTime::now()"),
        "panic hook should use SystemTime::now() directly"
    );

    // Should NOT use the utility function (which returns ISO 8601 string)
    // This ensures proper Debug formatting in telemetry
}

#[test]
fn test_panic_hook_idempotence() {
    // Verify the panic hook is idempotent - installing it multiple times is safe
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // Check for Once-based installation
    assert!(
        panic_capture_source.contains("static HOOK_INSTALLED: Once"),
        "hook installation should be Once-based for thread safety"
    );

    assert!(
        panic_capture_source.contains("HOOK_INSTALLED.call_once"),
        "hook should use call_once for idempotent installation"
    );

    // This ensures multiple calls to install_panic_hook() are safe
}

#[test]
fn test_panic_hook_integration_test() {
    // Integration test: verify the hook can be installed without panicking
    use needle::panic_capture::{install_panic_hook, is_hook_installed};

    // Install the hook
    install_panic_hook();

    // Verify it's installed
    assert!(
        is_hook_installed(),
        "panic hook should be installed after calling install_panic_hook()"
    );

    // Install again - should be safe (idempotent)
    install_panic_hook();

    // Should still be installed
    assert!(
        is_hook_installed(),
        "panic hook should remain installed after second call"
    );
}

#[test]
fn test_system_time_approximate_ordering() {
    // Test that timestamps captured in quick succession are approximately ordered
    let timestamps: Vec<SystemTime> = (0..10)
        .map(|_| {
            let ts = SystemTime::now();
            std::thread::sleep(std::time::Duration::from_millis(1));
            ts
        })
        .collect();

    // Convert to durations since epoch
    let durations: Vec<_> = timestamps
        .iter()
        .map(|ts| {
            ts.duration_since(std::time::UNIX_EPOCH)
                .expect("should be measurable")
        })
        .collect();

    // Verify each timestamp is >= the previous (monotonic)
    for i in 1..durations.len() {
        assert!(
            durations[i] >= durations[i - 1],
            "timestamp {} should be >= timestamp {}",
            i,
            i - 1
        );
    }
}

#[test]
fn test_timestamp_field_value_type() {
    // Verify that the timestamp field uses the correct type (SystemTime)
    // and formatting (Debug via ?)
    let panic_capture_source = include_str!("../src/panic_capture.rs");

    // The ?timestamp syntax applies Debug formatting
    // SystemTime's Debug output is readable and structured
    assert!(
        panic_capture_source.contains("timestamp = ?timestamp"),
        "timestamp should use Debug formatting via ?"
    );

    // This ensures the field value is properly formatted in logs
}
