//! File sink for JSONL telemetry.
//!
//! Writes structured events to `<log_dir>/<worker>-<session>.jsonl`.
//! Append-only, one line per event.

use anyhow::{Context, Result};
use chrono::Utc;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time;

use super::{Sink, TelemetryEvent};

/// Writes JSONL telemetry to `<log_dir>/<worker>-<session>.jsonl`.
///
/// Append-only, one line per event. The log directory is created if it
/// does not exist.
pub struct FileSink {
    /// Log directory where events are written.
    log_dir: PathBuf,
    /// Path to the specific log file for this worker/session.
    path: PathBuf,
    writer: Mutex<std::io::BufWriter<std::fs::File>>,
}

impl FileSink {
    /// Construct a sink using the default log directory (`~/.needle/logs/`).
    pub fn new(worker_id: &str, session_id: &str) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let log_dir = PathBuf::from(&home).join(".needle").join("logs");
        Self::with_dir(log_dir, worker_id, session_id)
    }

    /// Construct a sink writing to a specific directory.
    ///
    /// Creates the directory (and parents) if it does not exist.
    ///
    /// # Arguments
    /// * `log_dir` - Directory where log files will be written
    /// * `worker_id` - Worker identifier
    /// * `session_id` - Session identifier (8 hex chars)
    pub fn with_dir(log_dir: PathBuf, worker_id: &str, session_id: &str) -> Result<Self> {
        // Validate/log_dir exists or can be created
        if !log_dir.exists() {
            std::fs::create_dir_all(&log_dir)
                .with_context(|| format!("failed to create log directory: {}", log_dir.display()))?;
        }

        let filename = format!("{worker_id}-{session_id}.jsonl");
        let path = log_dir.join(filename);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open log file: {}", path.display()))?;

        Ok(FileSink {
            log_dir,
            path,
            writer: Mutex::new(std::io::BufWriter::new(file)),
        })
    }

    /// Return the log directory.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Return the path to the log file.
    pub fn path(&self) -> &Path {
        &self.path
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
        Self::write_boot_event_direct_impl(
            &self.writer,
            &self.path,
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
            Ok(Err(e)) => {
                Err(anyhow::anyhow!("failed to write boot event to {}: {}", path_for_error, e))
            }
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
        let line = serde_json::to_string(event)?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writeln!(writer, "{line}")?;
        writer.flush()?;
        Ok(())
    }

    fn flush(&self, _deadline: time::Duration) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {}", e))?;
        writer.flush()?;
        // fsync to ensure durability before shutdown
        writer.get_ref().sync_all()?;
        Ok(())
    }
}
