//! File sink for JSONL telemetry.
//!
//! Writes structured events to `<log_dir>/<worker>-<session>-<date>.jsonl`.
//! Supports daily and size-based file rotation.
//!
//! ## Rotation
//!
//! - **Daily rotation**: Creates a new file at midnight (UTC)
//! - **Size-based rotation**: Creates a new file when size limit is reached
//! - File naming: `{worker_id}-{session_id}-{date}.jsonl`
//!   - Date format: `YYYY-MM-DD`
//!   - Multiple files per day are numbered: `...-001.jsonl`, `...-002.jsonl`

use anyhow::{Context, Result};
use chrono::Utc;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicU64, Mutex};
use std::time;

use super::{Sink, TelemetryEvent};

/// Maximum size of a single log file before rotation (default: 100 MB)
const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Writes JSONL telemetry to `<log_dir>/<worker>-<session>-<date>.jsonl`.
///
/// Supports both daily and size-based file rotation. Events are written
/// atomically with proper error handling and file management.
pub struct FileSink {
    /// Log directory where events are written.
    log_dir: PathBuf,
    /// Worker identifier for file naming.
    worker_id: String,
    /// Session identifier for file naming.
    session_id: String,
    /// Current file path (for rotation tracking).
    current_path: Mutex<PathBuf>,
    /// Current file date (for daily rotation tracking).
    current_date: Mutex<String>,
    /// Current file sequence number (for multiple files per day).
    current_sequence: Mutex<u32>,
    /// Maximum file size before rotation (bytes).
    max_file_size: u64,
    /// Current file size estimate (atomic for non-blocking reads).
    current_size: AtomicU64,
    /// Buffered writer (replaced on rotation).
    writer: Mutex<std::io::BufWriter<std::fs::File>>,
}

impl FileSink {
    /// Construct a sink using the default log directory (`~/.needle/logs/`).
    ///
    /// Uses default rotation settings (100 MB max file size, daily rotation).
    pub fn new(worker_id: &str, session_id: &str) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let log_dir = PathBuf::from(&home).join(".needle").join("logs");
        Self::with_config(log_dir, worker_id, session_id, DEFAULT_MAX_FILE_SIZE_BYTES)
    }

    /// Construct a sink writing to a specific directory with custom size limit.
    ///
    /// Creates the directory (and parents) if it does not exist.
    ///
    /// # Arguments
    /// * `log_dir` - Directory where log files will be written
    /// * `worker_id` - Worker identifier
    /// * `session_id` - Session identifier (8 hex chars)
    /// * `max_file_size_bytes` - Maximum size of each log file before rotation
    pub fn with_config(
        log_dir: PathBuf,
        worker_id: &str,
        session_id: &str,
        max_file_size_bytes: u64,
    ) -> Result<Self> {
        // Validate/log_dir exists or can be created
        if !log_dir.exists() {
            std::fs::create_dir_all(&log_dir).with_context(|| {
                format!("failed to create log directory: {}", log_dir.display())
            })?;
        }

        let current_date = Utc::now().format("%Y-%m-%d").to_string();
        let filename = format!("{}-{}-{}.jsonl", worker_id, session_id, current_date);
        let path = log_dir.join(&filename);

        let (file, size) = Self::open_file_with_size(&path)?;
        let writer = std::io::BufWriter::with_capacity(64 * 1024, file);

        Ok(FileSink {
            log_dir,
            worker_id: worker_id.to_string(),
            session_id: session_id.to_string(),
            current_path: Mutex::new(path),
            current_date: Mutex::new(current_date),
            current_sequence: Mutex::new(1),
            max_file_size: max_file_size_bytes,
            current_size: AtomicU64::new(size),
            writer: Mutex::new(writer),
        })
    }

    /// Construct a sink writing to a specific directory (legacy compatibility).
    ///
    /// Creates the directory (and parents) if it does not exist.
    /// Uses default rotation settings.
    ///
    /// # Arguments
    /// * `log_dir` - Directory where log files will be written
    /// * `worker_id` - Worker identifier
    /// * `session_id` - Session identifier (8 hex chars)
    pub fn with_dir(log_dir: PathBuf, worker_id: &str, session_id: &str) -> Result<Self> {
        Self::with_config(log_dir, worker_id, session_id, DEFAULT_MAX_FILE_SIZE_BYTES)
    }

    /// Open a file and return it with its current size.
    ///
    /// If the file exists, appends to it. If not, creates a new file.
    fn open_file_with_size(path: &Path) -> Result<(std::fs::File, u64)> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        // Telemetry lines can carry sanitized transcript content, so restrict
        // newly created log files to the owner. This applies at creation only;
        // an existing file keeps its current mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .with_context(|| format!("failed to open log file: {}", path.display()))?;

        let size = file
            .metadata()
            .with_context(|| format!("failed to get metadata for: {}", path.display()))?
            .len();

        Ok((file, size))
    }

    /// Generate a new filename with date and sequence number.
    fn generate_filename(worker_id: &str, session_id: &str, date: &str, sequence: u32) -> String {
        format!(
            "{}-{}-{}-{:03}.jsonl",
            worker_id, session_id, date, sequence
        )
    }

    /// Rotate the log file (daily or size-based).
    ///
    /// Creates a new log file and updates the writer. Returns the new file path.
    fn rotate(&self) -> Result<PathBuf> {
        let now = Utc::now();
        let new_date = now.format("%Y-%m-%d").to_string();

        // Lock order: current_date -> current_sequence -> writer
        let mut current_date_guard = self
            .current_date
            .lock()
            .map_err(|e| anyhow::anyhow!("failed to acquire current_date lock: {}", e))?;

        // Check if we need daily rotation
        let needs_daily_rotation = *current_date_guard != new_date;

        if needs_daily_rotation {
            // New day, reset sequence number
            *current_date_guard = new_date.clone();
            let mut sequence_guard = self
                .current_sequence
                .lock()
                .map_err(|e| anyhow::anyhow!("failed to acquire current_sequence lock: {}", e))?;
            *sequence_guard = 1;
            drop(sequence_guard);
        }

        // Get next sequence number
        let mut sequence_guard = self
            .current_sequence
            .lock()
            .map_err(|e| anyhow::anyhow!("failed to acquire current_sequence lock: {}", e))?;
        let sequence = *sequence_guard;
        *sequence_guard += 1;
        drop(sequence_guard);
        drop(current_date_guard);

        // Generate new filename
        let filename = Self::generate_filename(
            &self.worker_id,
            &self.session_id,
            new_date.as_str(),
            sequence,
        );
        let new_path = self.log_dir.join(&filename);

        // Open new file
        let (new_file, size) = Self::open_file_with_size(&new_path)?;
        let new_writer = std::io::BufWriter::with_capacity(64 * 1024, new_file);

        // Update writer and path
        let mut writer_guard = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("failed to acquire writer lock: {}", e))?;

        // Flush old writer before replacing
        let _ = writer_guard.flush();

        // Replace writer
        *writer_guard = new_writer;

        // Update size and path
        self.current_size
            .store(size, std::sync::atomic::Ordering::Relaxed);
        let mut path_guard = self
            .current_path
            .lock()
            .map_err(|e| anyhow::anyhow!("failed to acquire current_path lock: {}", e))?;
        *path_guard = new_path.clone();
        drop(path_guard);

        Ok(new_path)
    }

    /// Check if rotation is needed and rotate if so.
    ///
    /// Returns the current file path (may be newly rotated).
    fn ensure_rotation(&self) -> Result<PathBuf> {
        let now = Utc::now();
        let current_date = now.format("%Y-%m-%d").to_string();

        // Check if daily rotation is needed
        {
            let date_guard = self
                .current_date
                .lock()
                .map_err(|e| anyhow::anyhow!("failed to acquire current_date lock: {}", e))?;
            if *date_guard != current_date {
                drop(date_guard);
                return self.rotate();
            }
        }

        // Check if size-based rotation is needed
        let current_size = self.current_size.load(std::sync::atomic::Ordering::Relaxed);
        if current_size >= self.max_file_size {
            return self.rotate();
        }

        // No rotation needed, return current path
        let path_guard = self
            .current_path
            .lock()
            .map_err(|e| anyhow::anyhow!("failed to acquire current_path lock: {}", e))?;
        Ok(path_guard.clone())
    }

    /// Return the log directory.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Return the current log file path.
    ///
    /// This may change after rotation.
    pub fn path(&self) -> PathBuf {
        let guard = self.current_path.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    }

    /// Get the current file size in bytes.
    pub fn current_file_size(&self) -> u64 {
        self.current_size.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Write a telemetry event to the file with automatic rotation.
    ///
    /// This method:
    /// 1. Checks if rotation is needed (daily or size-based)
    /// 2. Rotates to a new file if needed
    /// 3. Serializes the event to JSON
    /// 4. Writes the event to the current file
    /// 5. Updates the size counter
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - File rotation fails
    /// - JSON serialization fails
    /// - File write fails
    /// - Locks are poisoned
    pub fn write_event(&self, event: &TelemetryEvent) -> Result<()> {
        // Check and perform rotation if needed
        self.ensure_rotation()
            .context("failed to check/perform rotation")?;

        // Serialize event to JSON
        let line = serde_json::to_string(event).context("failed to serialize event to JSON")?;

        let line_bytes = line.as_bytes();
        let line_len = line_bytes.len() as u64 + 1; // +1 for newline

        // Write to file
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("writer lock poisoned: {}", e))?;

        writeln!(writer, "{}", line)
            .with_context(|| "failed to write event to log file".to_string())?;

        writer.flush().context("failed to flush log file")?;

        // Update size counter
        self.current_size
            .fetch_add(line_len, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    /// Write a boot event directly to the file, bypassing the normal channel.
    ///
    /// This is called immediately after FileSink creation to ensure that even if
    /// the writer thread fails to start, we have a trace in the JSONL file.
    /// The event is written synchronously and flushed to disk.
    pub fn write_boot_event_direct(
        &self,
        worker_id: &str,
        session_id: &str,
        version: &str,
    ) -> Result<()> {
        let current_path = self.path();
        Self::write_boot_event_direct_impl(
            &self.writer,
            &current_path,
            worker_id,
            session_id,
            version,
            time::Duration::from_secs(5), // 5 second timeout
        )
    }

    /// Write a boot event directly to the file with a timeout.
    ///
    /// This is a timeout-aware variant that prevents indefinite blocking on hung
    /// filesystems (e.g., network filesystem issues, stale NFS mounts). If the
    /// write takes longer than the timeout, it returns an error and the caller
    /// can decide whether to continue or fail.
    ///
    /// The timeout is implemented by spawning a thread to do the blocking I/O
    /// and joining with a timeout. If the timeout expires, the thread is detached
    /// and will continue running (and eventually complete or be killed by the OS).
    fn write_boot_event_direct_impl(
        _writer: &Mutex<std::io::BufWriter<std::fs::File>>,
        path: &Path,
        worker_id: &str,
        session_id: &str,
        version: &str,
        timeout: time::Duration,
    ) -> Result<()> {
        use std::sync::mpsc;
        use std::thread;

        let event = TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "worker.booting".to_string(),
            worker_id: worker_id.to_string(),
            session_id: session_id.to_string(),
            sequence: 0,
            bead_id: None,
            workspace: None,
            duration_ms: None,
            data: serde_json::json!({ "worker_name": worker_id, "version": version }),
            trace_id: None,
            span_id: None,
        };
        let line = serde_json::to_string(&event)?;
        let path_for_error = path.display().to_string();
        let path_clone = path.to_path_buf();

        // JoinHandle::join() has no timeout in Rust's std — use a channel with recv_timeout.
        let (tx, rx) = mpsc::channel::<Result<(), String>>();

        // The JoinHandle is intentionally dropped on timeout: dropping detaches the thread,
        // which continues running until it completes or the process exits.
        let _handle = thread::spawn(move || {
            let result: Result<(), String> = (|| {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path_clone)
                    .map_err(|e| e.to_string())?;
                let mut writer = std::io::BufWriter::new(file);
                writeln!(writer, "{line}").map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())?;
                writer.get_ref().sync_all().map_err(|e| e.to_string())?;
                Ok(())
            })();
            // Ignore send error: receiver may have timed out and been dropped.
            let _ = tx.send(result);
        });

        match rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(anyhow::anyhow!(
                "failed to write boot event to {}: {}",
                path_for_error,
                e
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow::anyhow!(
                "timed out writing boot event to {} after {:?} (filesystem may be hung)",
                path_for_error,
                timeout
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "boot event writer thread disconnected for {}",
                path_for_error
            )),
        }
    }
}

impl Sink for FileSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        // Use the new write_event method which handles rotation
        self.write_event(event)
    }

    fn flush(&self, _deadline: time::Duration) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("writer lock poisoned: {}", e))?;
        writer.flush()?;
        // fsync to ensure durability before shutdown
        writer.get_ref().sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    struct HomeGuard {
        previous: Option<OsString>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { previous }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("HOME test lock should not be poisoned")
    }

    fn create_test_event(worker_id: &str) -> TelemetryEvent {
        TelemetryEvent {
            timestamp: Utc::now(),
            event_type: "test.event".to_string(),
            worker_id: worker_id.to_string(),
            session_id: "test1234".to_string(),
            sequence: 1,
            bead_id: None,
            workspace: None,
            duration_ms: None,
            data: serde_json::json!({ "test": "data" }),
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn test_file_sink_writes_jsonl_with_required_fields() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink = FileSink::with_config(
            log_dir.to_path_buf(),
            "test-worker",
            "sess001",
            1024 * 1024, // 1 MB max
        )
        .unwrap();

        let event = create_test_event("test-worker");
        sink.write_event(&event).unwrap();

        // Verify file was created
        let current_path = sink.path();
        assert!(current_path.exists());

        // Each event is one newline-delimited JSON object.
        let content = fs::read_to_string(&current_path).unwrap();
        assert!(content.ends_with('\n'));
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        for field in [
            "timestamp",
            "event_type",
            "worker_id",
            "session_id",
            "sequence",
            "data",
        ] {
            assert!(
                value.get(field).is_some(),
                "missing required field: {field}"
            );
        }

        let parsed: TelemetryEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.timestamp, event.timestamp);
        assert_eq!(parsed.event_type, "test.event");
        assert_eq!(parsed.worker_id, "test-worker");
        assert_eq!(parsed.session_id, "test1234");
        assert_eq!(parsed.sequence, 1);
        assert_eq!(parsed.data, serde_json::json!({ "test": "data" }));
    }

    #[test]
    fn test_file_sink_creates_log_directory() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().join("nested").join("logs");

        assert!(!log_dir.exists());

        let sink =
            FileSink::with_config(log_dir.clone(), "test-worker", "sess001", 1024 * 1024).unwrap();

        assert!(log_dir.exists());
        assert!(sink.path().exists());
    }

    #[test]
    fn test_file_sink_size_based_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        // Set a very small size limit to trigger rotation
        let sink = FileSink::with_config(
            log_dir.to_path_buf(),
            "test-worker",
            "sess001",
            500, // 500 bytes max
        )
        .unwrap();

        let event = create_test_event("test-worker");
        let initial_path = sink.path();

        // Write events until rotation occurs
        let mut write_count = 0;
        for _ in 0..10 {
            sink.write_event(&event).unwrap();
            write_count += 1;

            // Check if path changed (rotation occurred)
            let new_path = sink.path();
            if new_path != initial_path {
                break;
            }
        }

        // Rotation should have occurred
        assert_ne!(sink.path(), initial_path);
        assert!(write_count > 1, "Should write at least 2 events");

        // Verify both files exist
        assert!(initial_path.exists());
        assert!(sink.path().exists());
    }

    #[test]
    fn test_file_sink_daily_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess001", 1024 * 1024)
                .unwrap();

        let initial_path = sink.path();
        let initial_date = Utc::now().format("%Y-%m-%d").to_string();

        // Verify initial filename contains date
        assert!(initial_path.to_string_lossy().contains(&initial_date));

        // Write an event
        let event = create_test_event("test-worker");
        sink.write_event(&event).unwrap();

        // Mock a date change by manipulating the internal state
        // This tests the rotation logic without waiting for actual midnight
        {
            let mut current_date = sink.current_date.lock().unwrap();
            *current_date = "2000-01-01".to_string();
        }

        // Write another event - this should trigger rotation
        sink.write_event(&event).unwrap();

        // Verify path changed (rotation occurred)
        let new_path = sink.path();
        assert_ne!(new_path, initial_path);
        // Rotation names the new file for the current date, not the stale one.
        assert!(new_path.to_string_lossy().contains(&initial_date));
    }

    #[test]
    fn test_file_sink_sequence_numbering() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink = FileSink::with_config(
            log_dir.to_path_buf(),
            "test-worker",
            "sess001",
            500, // Small size to force multiple rotations
        )
        .unwrap();

        let event = create_test_event("test-worker");

        // Write enough events to trigger multiple rotations
        for _ in 0..20 {
            let _ = sink.write_event(&event);
        }

        // Check for multiple files in the log directory
        let files: Vec<_> = fs::read_dir(log_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .collect();

        // Should have created multiple files due to rotation
        assert!(files.len() >= 2, "Should have at least 2 rotated files");
    }

    #[test]
    fn test_file_sink_current_file_size() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess001", 1024 * 1024)
                .unwrap();

        assert_eq!(sink.current_file_size(), 0);

        let event = create_test_event("test-worker");
        sink.write_event(&event).unwrap();

        // Size should have increased
        assert!(sink.current_file_size() > 0);

        let initial_size = sink.current_file_size();
        sink.write_event(&event).unwrap();

        // Size should have increased again
        assert!(sink.current_file_size() > initial_size);
    }

    #[test]
    fn test_file_sink_error_handling() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess001", 1024 * 1024)
                .unwrap();

        // Test that write errors are propagated
        // Create an event with problematic data (if applicable)
        let event = create_test_event("test-worker");

        // Write should succeed
        assert!(sink.write_event(&event).is_ok());

        // Flush should succeed
        assert!(sink.flush(time::Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn test_file_sink_sink_trait() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess001", 1024 * 1024)
                .unwrap();

        let event = create_test_event("test-worker");

        // Test Sink trait accept() method
        assert!(sink.accept(&event).is_ok());

        // Verify file was written
        let path = sink.path();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("test.event"));
    }

    #[test]
    fn test_file_sink_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess001", 1024 * 1024)
                .unwrap();

        let event = create_test_event("test-worker");
        sink.write_event(&event).unwrap();

        let path = sink.path();
        let metadata = fs::metadata(&path).unwrap();

        // Check file is readable and writable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = metadata.permissions();
            let mode = permissions.mode();

            // File should be readable and writable by owner (0o600)
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn test_file_sink_append_mode() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        // Create initial file with some content. Per the module docs the first
        // file of a day is `{worker}-{session}-{date}.jsonl`; the `-001`
        // suffix is only used for subsequent rotations within the same day.
        let filename = format!(
            "test-worker-sess002-{}.jsonl",
            Utc::now().format("%Y-%m-%d")
        );
        let path = log_dir.join(&filename);

        fs::create_dir_all(log_dir).unwrap();
        fs::write(&path, "{\"old\":\"data\"}\n").unwrap();

        // Create sink that should append to existing file
        let sink =
            FileSink::with_config(log_dir.to_path_buf(), "test-worker", "sess002", 1024 * 1024)
                .unwrap();

        let event = create_test_event("test-worker");
        sink.write_event(&event).unwrap();

        // Verify both old and new content exist
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"old\":\"data\""));
        assert!(content.contains("test.event"));
    }

    #[test]
    fn test_file_sink_new_uses_isolated_home_log_dir() {
        let temp_dir = TempDir::new().unwrap();
        let _home_lock = home_lock();
        let _home = HomeGuard::set(temp_dir.path());

        let sink = FileSink::new("test-worker", "sess003").unwrap();

        assert_eq!(
            sink.log_dir(),
            temp_dir.path().join(".needle").join("logs").as_path()
        );
        assert!(sink.log_dir().is_dir());
        assert!(sink.path().is_file());
        assert_eq!(sink.max_file_size, DEFAULT_MAX_FILE_SIZE_BYTES);
    }
}
