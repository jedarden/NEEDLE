//! Test utilities for the telemetry module.

use crate::telemetry::{Result, Sink, TelemetryEvent};
use std::sync::{Arc, Mutex};

/// In-memory sink for testing — collects events via a shared Vec.
pub struct MemorySink {
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

impl MemorySink {
    pub fn new() -> (Self, Arc<Mutex<Vec<TelemetryEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            MemorySink {
                events: events.clone(),
            },
            events,
        )
    }
}

impl Sink for MemorySink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn flush(&self, _deadline: std::time::Duration) -> Result<()> {
        Ok(())
    }
}
