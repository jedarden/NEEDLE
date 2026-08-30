//! Query and aggregation for stored telemetry logs.
//!
//! Provides functionality to read and filter telemetry events from
//! JSONL log files, with support for worker filtering, time range filtering,
//! event type filtering, and per-worker statistics aggregation.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::TelemetryEvent;

/// Filter criteria for querying telemetry events.
#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    /// Filter by worker ID (optional).
    pub worker_id: Option<String>,
    /// Filter by session ID (optional).
    pub session_id: Option<String>,
    /// Filter by event type (optional).
    pub event_type: Option<String>,
    /// Filter by bead ID (optional).
    pub bead_id: Option<String>,
    /// Start of time range (inclusive, optional).
    pub start_time: Option<DateTime<Utc>>,
    /// End of time range (inclusive, optional).
    pub end_time: Option<DateTime<Utc>>,
    /// Maximum number of events to return (optional).
    pub limit: Option<usize>,
}

/// Result of a telemetry query.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Matching events.
    pub events: Vec<TelemetryEvent>,
    /// Total events matched (before limit).
    pub total_matched: usize,
    /// Log files scanned.
    pub files_scanned: usize,
}

/// Per-worker statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStats {
    /// Worker ID.
    pub worker_id: String,
    /// Session ID.
    pub session_id: String,
    /// Total events for this worker.
    pub event_count: u64,
    /// Count by event type.
    pub event_types: HashMap<String, u64>,
    /// First event timestamp.
    pub first_event: Option<DateTime<Utc>>,
    /// Last event timestamp.
    pub last_event: Option<DateTime<Utc>>,
    /// Beads processed.
    pub beads_processed: HashSet<String>,
    /// Last bead processed (if any).
    pub last_bead: Option<String>,
}

/// Aggregate statistics across all workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateStats {
    /// Statistics per worker.
    pub workers: Vec<WorkerStats>,
    /// Total events across all workers.
    pub total_events: u64,
    /// Total workers active.
    pub total_workers: usize,
    /// Time range of data.
    pub earliest_event: Option<DateTime<Utc>>,
    pub latest_event: Option<DateTime<Utc>>,
}

/// Discover telemetry log files in a directory.
///
/// Returns paths to all JSONL files matching the pattern:
/// `{worker}-{session}-{date}.jsonl` or `{worker}-{session}-{date}-{seq}.jsonl`
pub fn discover_log_files(log_dir: &Path) -> Result<Vec<PathBuf>> {
    if !log_dir.exists() {
        return Ok(Vec::new());
    }

    let mut log_files = Vec::new();
    let entries = fs::read_dir(log_dir)
        .with_context(|| format!("failed to read log directory: {}", log_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        // Only process files ending in .jsonl
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        // Skip if not a file
        if !path.is_file() {
            continue;
        }

        // Filter to only include standard NEEDLE telemetry files
        // Pattern: {worker}-{session}-{date}.jsonl or {worker}-{session}-{date}-{seq}.jsonl
        // Exclude .agent.jsonl and other non-standard files
        let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if filename.ends_with(".agent") {
            continue;
        }

        log_files.push(path);
    }

    // Sort by modification time (newest first) for efficient querying
    log_files.sort_by_key(|p| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    log_files.reverse();

    Ok(log_files)
}

/// Query telemetry events from a log directory.
///
/// Applies filters and returns matching events with statistics.
pub fn query_logs(log_dir: &Path, filter: &QueryFilter) -> Result<QueryResult> {
    let log_files = discover_log_files(log_dir)?;
    let mut matched_events = Vec::new();
    let mut total_matched = 0;

    for path in &log_files {
        let file_events = read_events_from_file(path, filter)?;
        total_matched += file_events.len();

        // Apply limit if specified
        if let Some(limit) = filter.limit {
            if matched_events.len() + file_events.len() > limit {
                let remaining = limit - matched_events.len();
                matched_events.extend(file_events.into_iter().take(remaining));
                break;
            }
        }

        matched_events.extend(file_events);
    }

    Ok(QueryResult {
        events: matched_events,
        total_matched,
        files_scanned: log_files.len(),
    })
}

/// Read events from a single file, applying filters.
fn read_events_from_file(path: &Path, filter: &QueryFilter) -> Result<Vec<TelemetryEvent>> {
    let file = fs::File::open(path)
        .with_context(|| format!("failed to open log file: {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let event: TelemetryEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue, // Skip malformed lines
        };

        // Apply filters
        if !event_matches_filter(&event, filter) {
            continue;
        }

        events.push(event);
    }

    Ok(events)
}

/// Check if an event matches the given filter.
fn event_matches_filter(event: &TelemetryEvent, filter: &QueryFilter) -> bool {
    // Worker ID filter
    if let Some(ref worker_id) = filter.worker_id {
        if &event.worker_id != worker_id {
            return false;
        }
    }

    // Session ID filter
    if let Some(ref session_id) = filter.session_id {
        if &event.session_id != session_id {
            return false;
        }
    }

    // Event type filter
    if let Some(ref event_type) = filter.event_type {
        if !event.event_type.contains(event_type) {
            return false;
        }
    }

    // Bead ID filter
    if let Some(ref bead_id) = filter.bead_id {
        if event.bead_id.as_ref().map(|b| b.to_string()) != Some(bead_id.clone()) {
            return false;
        }
    }

    // Time range filters
    if let Some(start) = filter.start_time {
        if event.timestamp < start {
            return false;
        }
    }

    if let Some(end) = filter.end_time {
        if event.timestamp > end {
            return false;
        }
    }

    true
}

/// Compute aggregate statistics from log files.
pub fn compute_stats(log_dir: &Path, worker_filter: Option<&str>) -> Result<AggregateStats> {
    let log_files = discover_log_files(log_dir)?;
    let mut worker_stats_map: HashMap<String, WorkerStats> = HashMap::new();
    let mut total_events = 0u64;
    let mut earliest: Option<DateTime<Utc>> = None;
    let mut latest: Option<DateTime<Utc>> = None;

    for path in &log_files {
        let file = fs::File::open(path)
            .with_context(|| format!("failed to open log file: {}", path.display()))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };

            let event: TelemetryEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue, // Skip malformed lines silently
            };

            // Apply worker filter if specified
            if let Some(filter) = worker_filter {
                if !event.worker_id.contains(filter) {
                    continue;
                }
            }

            total_events += 1;

            // Update time range
            earliest = Some(earliest.map_or(event.timestamp, |e| e.min(event.timestamp)));
            latest = Some(latest.map_or(event.timestamp, |l| l.max(event.timestamp)));

            // Get or create worker stats
            let worker_key = format!("{}-{}", event.worker_id, event.session_id);
            let stats = worker_stats_map
                .entry(worker_key.clone())
                .or_insert_with(|| WorkerStats {
                    worker_id: event.worker_id.clone(),
                    session_id: event.session_id.clone(),
                    event_count: 0,
                    event_types: HashMap::new(),
                    first_event: None,
                    last_event: None,
                    beads_processed: HashSet::new(),
                    last_bead: None,
                });

            stats.event_count += 1;
            stats.first_event = Some(
                stats
                    .first_event
                    .map_or(event.timestamp, |f| f.min(event.timestamp)),
            );
            stats.last_event = Some(
                stats
                    .last_event
                    .map_or(event.timestamp, |l| l.max(event.timestamp)),
            );

            // Count by event type
            *stats
                .event_types
                .entry(event.event_type.clone())
                .or_insert(0) += 1;

            // Track beads
            if let Some(ref bead_id) = event.bead_id {
                stats.beads_processed.insert(bead_id.to_string());
                stats.last_bead = Some(bead_id.to_string());
            }
        }
    }

    // Convert to sorted vector
    let mut workers: Vec<_> = worker_stats_map.into_values().collect();
    workers.sort_by(|a, b| {
        // Sort by last activity (most recent first)
        match (&a.last_event, &b.last_event) {
            (Some(aa), Some(bb)) => bb.cmp(aa),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.worker_id.cmp(&b.worker_id),
        }
    });

    Ok(AggregateStats {
        total_workers: workers.len(),
        workers,
        total_events,
        earliest_event: earliest,
        latest_event: latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_event(worker_id: &str, event_type: &str) -> TelemetryEvent {
        TelemetryEvent {
            timestamp: Utc::now(),
            event_type: event_type.to_string(),
            worker_id: worker_id.to_string(),
            session_id: "test1234".to_string(),
            sequence: 1,
            bead_id: Some("test-bead-1".into()),
            workspace: Some(PathBuf::from("/test/workspace")),
            data: serde_json::json!({ "test": "data" }),
            duration_ms: None,
            trace_id: None,
            span_id: None,
        }
    }

    fn write_test_log(path: &Path, events: &[TelemetryEvent]) -> Result<()> {
        let mut file = fs::File::create(path)?;
        for event in events {
            let line = serde_json::to_string(event)?;
            writeln!(file, "{}", line)?;
        }
        file.flush()?;
        Ok(())
    }

    #[test]
    fn test_discover_log_files() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        // Create test log files
        let log1 = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let log2 = log_dir.join("worker2-sess002-2024-01-01.jsonl");
        let not_json = log_dir.join("readme.txt");

        fs::write(&log1, "test").unwrap();
        fs::write(&log2, "test").unwrap();
        fs::write(&not_json, "test").unwrap();

        let files = discover_log_files(log_dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&log1));
        assert!(files.contains(&log2));
    }

    #[test]
    fn test_query_logs_with_worker_filter() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let log_path = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let events = vec![
            create_test_event("worker1", "test.event"),
            create_test_event("worker2", "test.event"),
        ];
        write_test_log(&log_path, &events).unwrap();

        let filter = QueryFilter {
            worker_id: Some("worker1".to_string()),
            ..Default::default()
        };

        let result = query_logs(log_dir, &filter).unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].worker_id, "worker1");
        assert_eq!(result.total_matched, 1);
    }

    #[test]
    fn test_query_logs_with_event_type_filter() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let log_path = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let events = vec![
            create_test_event("worker1", "worker.started"),
            create_test_event("worker1", "claim.success"),
        ];
        write_test_log(&log_path, &events).unwrap();

        let filter = QueryFilter {
            event_type: Some("claim".to_string()),
            ..Default::default()
        };

        let result = query_logs(log_dir, &filter).unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].event_type, "claim.success");
    }

    #[test]
    fn test_query_logs_with_limit() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let log_path = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let events = vec![
            create_test_event("worker1", "event.1"),
            create_test_event("worker1", "event.2"),
            create_test_event("worker1", "event.3"),
        ];
        write_test_log(&log_path, &events).unwrap();

        let filter = QueryFilter {
            limit: Some(2),
            ..Default::default()
        };

        let result = query_logs(log_dir, &filter).unwrap();
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.total_matched, 3); // Total matched before limit
    }

    #[test]
    fn test_compute_stats() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let log_path = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let events = vec![
            create_test_event("worker1", "worker.started"),
            create_test_event("worker1", "claim.success"),
            create_test_event("worker1", "claim.success"),
        ];
        write_test_log(&log_path, &events).unwrap();

        let stats = compute_stats(log_dir, None).unwrap();
        assert_eq!(stats.total_workers, 1);
        assert_eq!(stats.total_events, 3);

        let worker = &stats.workers[0];
        assert_eq!(worker.worker_id, "worker1");
        assert_eq!(worker.event_count, 3);
        assert_eq!(worker.event_types.get("claim.success"), Some(&2));
        assert_eq!(worker.beads_processed.len(), 1);
    }

    #[test]
    fn test_compute_stats_with_worker_filter() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let log1 = log_dir.join("worker1-sess001-2024-01-01.jsonl");
        let log2 = log_dir.join("worker2-sess002-2024-01-01.jsonl");

        write_test_log(&log1, &[create_test_event("worker1", "test.event")]).unwrap();
        write_test_log(&log2, &[create_test_event("worker2", "test.event")]).unwrap();

        let stats = compute_stats(log_dir, Some("worker1")).unwrap();
        assert_eq!(stats.total_workers, 1);
        assert_eq!(stats.workers[0].worker_id, "worker1");
    }

    #[test]
    fn test_query_logs_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let result = query_logs(log_dir, &QueryFilter::default()).unwrap();
        assert_eq!(result.events.len(), 0);
        assert_eq!(result.files_scanned, 0);
    }
}
