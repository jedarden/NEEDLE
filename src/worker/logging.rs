//! Structured logging helpers for the worker module.
//!
//! This module provides convenient logging functions that emit structured
//! log events through the telemetry system. All logs include automatic
//! context like worker_id, timestamp, and phase information.

use crate::telemetry::{EventKind, Telemetry};
use crate::types::BeadId;
use serde_json::json;
use std::collections::HashMap;

/// A structured log event with phase, timestamp, and context.
///
/// This type represents a log entry that will be emitted through the
/// telemetry system. The timestamp is captured when the event is created.
#[derive(Debug, Clone)]
pub struct LogEvent {
    /// Phase identifier (e.g., "dispatch", "claim", "outcome")
    pub phase: String,
    /// Timestamp when the log event was created
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Structured context data as key-value pairs
    pub context: HashMap<String, String>,
}

/// Log levels for structured logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Informational message
    Info,
    /// Warning message
    Warn,
    /// Error message
    Error,
    /// Debug message
    Debug,
}

impl LogLevel {
    /// Convert to string representation for telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Debug => "debug",
        }
    }
}

/// Emit a structured log event through the telemetry system at the specified level.
///
/// This is the core logging function that all other helpers build upon.
/// It converts the LogEvent into a telemetry EventKind and emits it at the
/// specified log level.
///
/// # Arguments
///
/// * `telemetry` - The telemetry instance to emit through
/// * `event` - The log event to emit
/// * `level` - The log level to emit at
/// * `bead_id` - Optional bead identifier for context tracking
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::{LogEvent, emit_log_with_level, LogLevel};
/// # use needle::telemetry::Telemetry;
/// # use std::collections::HashMap;
/// # let telemetry: Telemetry = unimplemented!();
/// let mut context = HashMap::new();
/// context.insert("status".to_string(), "started".to_string());
/// let event = LogEvent {
///     phase: "dispatch".to_string(),
///     timestamp: chrono::Utc::now(),
///     context,
/// };
/// emit_log_with_level(&telemetry, &event, LogLevel::Info, None).ok();
/// ```
pub fn emit_log_with_level(
    telemetry: &Telemetry,
    event: &LogEvent,
    level: LogLevel,
    bead_id: Option<&BeadId>,
) -> anyhow::Result<()> {
    // Convert HashMap<String, String> to serde_json::Value for telemetry
    let json_context = serde_json::Value::Object(
        event.context
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    );
    telemetry.log(&event.phase, level.as_str(), json_context, bead_id.cloned())
}

/// Emit a structured log event through the telemetry system at info level.
///
/// This is a convenience function that calls `emit_log_with_level` with `LogLevel::Info`.
pub fn emit_log(telemetry: &Telemetry, event: &LogEvent) -> anyhow::Result<()> {
    emit_log_with_level(telemetry, event, LogLevel::Info, None)
}

/// Emit a structured log event with phase and context.
///
/// This is a convenience function that builds a LogEvent internally
/// with the current timestamp and emits it through the telemetry system
/// at the Info level (for backward compatibility). For explicit level control,
/// use `emit_log_event_with_level()`.
///
/// # Arguments
///
/// * `telemetry` - The telemetry instance to emit through
/// * `phase` - The phase identifier (e.g., "dispatch", "claim", "outcome")
/// * `context` - Slice of key-value pairs for structured context data
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::emit_log_event;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// emit_log_event(&telemetry, "dispatch", &[("status", "started"), ("worker_id", "worker-1")]).ok();
/// ```
pub fn emit_log_event(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
) -> anyhow::Result<()> {
    emit_log_event_with_level(telemetry, phase, context, LogLevel::Info)
}

/// Emit a structured log event with phase, context, and explicit log level.
///
/// This is a convenience function that builds a LogEvent internally
/// with the current timestamp and emits it through the telemetry system
/// at the specified log level.
///
/// # Arguments
///
/// * `telemetry` - The telemetry instance to emit through
/// * `phase` - The phase identifier (e.g., "dispatch", "claim", "outcome")
/// * `context` - Slice of key-value pairs for structured context data
/// * `level` - The log level to emit at
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::{emit_log_event_with_level, LogLevel};
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// emit_log_event_with_level(&telemetry, "dispatch", &[("status", "started")], LogLevel::Info).ok();
/// ```
pub fn emit_log_event_with_level(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
    level: LogLevel,
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, level, None)
}

/// Convenience macro for emitting structured log events with key-value syntax.
///
/// This macro provides an ergonomic way to log events without manually building
/// a HashMap or LogEvent struct. It automatically captures the current timestamp
/// and converts all values to strings.
///
/// # Arguments
///
/// * `$telemetry` - The telemetry instance to emit through
/// * `$phase` - The phase identifier (e.g., "dispatch", "claim", "outcome")
/// * `$($key:expr => $val:expr),*` - Key-value pairs for structured context data
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_event;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// // Basic usage
/// log_event!(&telemetry, "dispatch", "status" => "started");
///
/// // Multiple context fields
/// log_event!(&telemetry, "claim",
///     "attempt" => "1",
///     "bead_id" => "bf-123",
///     "result" => "success"
/// );
///
/// // With trailing comma (optional)
/// log_event!(&telemetry, "outcome", "exit_code" => "0",);
///
/// // Values are automatically converted to strings
/// log_event!(&telemetry, "routing", "model_count" => 5);
/// ```
///
/// The macro returns `anyhow::Result<()>` so you can use `.ok()` or `?` as needed.
#[macro_export]
macro_rules! log_event {
    ($telemetry:expr, $phase:expr, $($key:expr => $val:expr),* $(,)?) => {{
        let mut context = std::collections::HashMap::new();
        $(
            context.insert($key.to_string(), $val.to_string());
        )*
        $crate::worker::logging::emit_log_event($telemetry, $phase, &context.into_iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>())
    }};
}

/// Emit an info-level log with the given phase and context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_info;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// log_info(&telemetry, "dispatch", &[("status", "started")]).ok();
/// ```
pub fn log_info(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Info, None)
}

/// Emit a warn-level log with the given phase and context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_warn;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// log_warn(&telemetry, "claim", &[("reason", "race_lost")]).ok();
/// ```
pub fn log_warn(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Warn, None)
}

/// Emit an error-level log with the given phase and context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_error;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// log_error(&telemetry, "outcome", &[("exit_code", "1")]).ok();
/// ```
pub fn log_error(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Error, None)
}

/// Emit a debug-level log with the given phase and context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_debug;
/// # use needle::telemetry::Telemetry;
/// # let telemetry: Telemetry = unimplemented!();
/// log_debug(&telemetry, "routing", &[("model", "sonnet-4")]).ok();
/// ```
pub fn log_debug(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Debug, None)
}

/// Emit an info-level log with bead context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_info_with_bead;
/// # use needle::telemetry::Telemetry;
/// # use needle::types::BeadId;
/// # let telemetry: Telemetry = unimplemented!();
/// # let bead_id = BeadId::from("test");
/// log_info_with_bead(&telemetry, "claim", &[("attempt", "1")], &bead_id).ok();
/// ```
pub fn log_info_with_bead(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
    bead_id: &BeadId,
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Info, Some(bead_id))
}

/// Emit a warn-level log with bead context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_warn_with_bead;
/// # use needle::telemetry::Telemetry;
/// # use needle::types::BeadId;
/// # let telemetry: Telemetry = unimplemented!();
/// # let bead_id = BeadId::from("test");
/// log_warn_with_bead(&telemetry, "claim", &[("reason", "race_lost")], &bead_id).ok();
/// ```
pub fn log_warn_with_bead(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
    bead_id: &BeadId,
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Warn, Some(bead_id))
}

/// Emit an error-level log with bead context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_error_with_bead;
/// # use needle::telemetry::Telemetry;
/// # use needle::types::BeadId;
/// # let telemetry: Telemetry = unimplemented!();
/// # let bead_id = BeadId::from("test");
/// log_error_with_bead(&telemetry, "outcome", &[("exit_code", "1")], &bead_id).ok();
/// ```
pub fn log_error_with_bead(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
    bead_id: &BeadId,
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Error, Some(bead_id))
}

/// Emit a debug-level log with bead context.
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::log_debug_with_bead;
/// # use needle::telemetry::Telemetry;
/// # use needle::types::BeadId;
/// # let telemetry: Telemetry = unimplemented!();
/// # let bead_id = BeadId::from("test");
/// log_debug_with_bead(&telemetry, "routing", &[("model", "sonnet-4")], &bead_id).ok();
/// ```
pub fn log_debug_with_bead(
    telemetry: &Telemetry,
    phase: &str,
    context: &[(&str, &str)],
    bead_id: &BeadId,
) -> anyhow::Result<()> {
    let context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log_with_level(telemetry, &event, LogLevel::Debug, Some(bead_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::test_utils::MemorySink;
    use crate::types::WorkerId;

    #[test]
    fn test_log_event_creation() {
        let mut context = HashMap::new();
        context.insert("key".to_string(), "value".to_string());
        let event = LogEvent {
            phase: "test".to_string(),
            timestamp: chrono::Utc::now(),
            context,
        };
        assert_eq!(event.phase, "test");
        assert_eq!(event.context.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_log_info() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_info(&telemetry, "test_phase", &[("key", "value")])
            .unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
    }

    #[test]
    fn test_log_error() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_error(&telemetry, "error_phase", &[("error_code", "500")])
            .unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
    }

    #[test]
    fn test_log_with_bead() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );
        let bead_id = BeadId::from("test-bead");

        log_info_with_bead(&telemetry, "claim", &[("attempt", "1")], &bead_id)
            .unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        // Bead ID should now be in the context
        assert!(events[0].data.get("bead_id").is_some());
    }

    #[test]
    fn test_emit_log_event() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        emit_log_event(&telemetry, "test_phase", &[("key1", "value1"), ("key2", "value2")])
            .unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        // Verify context data is present
        assert!(events[0].data.get("key1").is_some());
        assert!(events[0].data.get("key2").is_some());
    }

    #[test]
    fn test_emit_log_event_empty_context() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        emit_log_event(&telemetry, "test_phase", &[])
            .unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
    }

    #[test]
    fn test_log_event_macro() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_event!(&telemetry, "dispatch", "status" => "started").unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        assert!(events[0].data.get("status").is_some());
    }

    #[test]
    fn test_log_event_macro_multiple_fields() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_event!(&telemetry, "claim",
            "attempt" => "1",
            "bead_id" => "bf-123",
            "result" => "success"
        ).unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        assert!(events[0].data.get("attempt").is_some());
        assert!(events[0].data.get("bead_id").is_some());
        assert!(events[0].data.get("result").is_some());
    }

    #[test]
    fn test_log_event_macro_with_trailing_comma() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_event!(&telemetry, "outcome", "exit_code" => "0",).unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        assert!(events[0].data.get("exit_code").is_some());
    }

    #[test]
    fn test_log_event_macro_auto_string_conversion() {
        let sink = MemorySink::new();
        let telemetry = Telemetry::new(
            WorkerId::new("test-worker"),
            "test-session",
            std::sync::Arc::new(sink.clone()),
        );

        log_event!(&telemetry, "routing",
            "model_count" => 5,
            "duration_ms" => 1234
        ).unwrap();

        let events = sink.collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "log.entry");
        // Numbers should be converted to strings
        assert_eq!(events[0].data.get("model_count"), Some(&serde_json::Value::String("5".to_string())));
        assert_eq!(events[0].data.get("duration_ms"), Some(&serde_json::Value::String("1234".to_string())));
    }
}
