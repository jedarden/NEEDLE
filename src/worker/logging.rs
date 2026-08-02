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

/// Emit a structured log event through the telemetry system.
///
/// This is the core logging function that all other helpers build upon.
/// It converts the LogEvent into a telemetry EventKind and emits it.
///
/// # Arguments
///
/// * `telemetry` - The telemetry instance to emit through
/// * `event` - The log event to emit
///
/// # Example
///
/// ```no_run
/// # use needle::worker::logging::{LogEvent, emit_log};
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
/// emit_log(&telemetry, &event).ok();
/// ```
pub fn emit_log(telemetry: &Telemetry, event: &LogEvent) -> anyhow::Result<()> {
    // Convert HashMap<String, String> to serde_json::Value for telemetry
    let json_context = serde_json::Value::Object(
        event.context
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    );
    telemetry.log_info(&event.phase, json_context)
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
    emit_log(telemetry, &event)
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
    emit_log(telemetry, &event)
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
    emit_log(telemetry, &event)
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
    emit_log(telemetry, &event)
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
    let mut context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    context_map.insert("bead_id".to_string(), bead_id.to_string());
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log(telemetry, &event)
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
    let mut context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    context_map.insert("bead_id".to_string(), bead_id.to_string());
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log(telemetry, &event)
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
    let mut context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    context_map.insert("bead_id".to_string(), bead_id.to_string());
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log(telemetry, &event)
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
    let mut context_map = context
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>();
    context_map.insert("bead_id".to_string(), bead_id.to_string());
    let event = LogEvent {
        phase: phase.to_string(),
        timestamp: chrono::Utc::now(),
        context: context_map,
    };
    emit_log(telemetry, &event)
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
}
