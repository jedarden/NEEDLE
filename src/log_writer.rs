//! Size-capped rolling writer for worker logs.
//!
//! # Why this exists
//!
//! `tracing-appender` rotates on **time only** — its `Rotation` is a closed enum of
//! `MINUTELY | HOURLY | DAILY | NEVER`, with no byte-based variant. That is not a
//! bound. On 2026-08-05 a span leak (bf-3uj6i) produced ~159 GB/hr of output; with
//! hourly rotation the *current* file passes 159 GB before it ever rotates, and
//! `max_log_files` caps file count rather than size. A 444 GB disk still fills in
//! under three hours.
//!
//! This writer bounds total bytes instead: `max_bytes × (max_files + 1)`, which is
//! the number an operator actually cares about — "NEEDLE cannot exceed N".
//!
//! # Line bound
//!
//! Byte rotation alone does not bound a single event: one leaked span stack
//! can format a multi-hundred-KiB line in a single call, which is why
//! [`DEFAULT_MAX_LINE_BYTES`] exists alongside the roll threshold. Every line
//! passing through [`LineCappedMakeWriter`] is at most `max_line_bytes` bytes
//! *including* its terminating newline — the formatter's payload gets
//! `max_line_bytes - 1`, and the newline always fits. Bytes past the limit on
//! a line are discarded and reported as written so the formatter never
//! retries them; full output resumes at the next newline. Truncation is
//! deterministic (input line N maps to output line N) and cannot fail — an
//! oversized event degrades to a short line, never to an error or an
//! unbounded write.
//!
//! In production the cap is applied at the writer boundary, so it holds
//! wherever a line is headed: `worker_log_writer` (in `cli`) wraps both the
//! rolling [`SizeCappedWriter`] file *and* the stderr fallback in
//! [`LineCappedMakeWriter`]. The structured JSONL that `telemetry::FileSink`
//! writes is a separate path and is intentionally not capped — the bound is
//! for the human-readable stream, not for structured telemetry.
//!
//! # What it deliberately does not do
//!
//! It does not fix whatever is producing the volume. A cap turns a disk-filling
//! runaway into a silent one — the machine keeps burning write bandwidth and CPU,
//! and the *early* diagnostic context is the first thing discarded. So it also
//! tracks roll frequency and warns when rolling gets pathological, which is the
//! signal that would have surfaced bf-3uj6i in minutes rather than after a
//! full disk.
//!
//! The warning goes to real stderr via `eprintln!`, never `tracing::warn!` —
//! emitting a tracing event from inside the writer that serves tracing would
//! recurse.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// Maximum bytes emitted for one human-readable tracing line.
///
/// This is deliberately much smaller than the file-roll threshold. A disk cap
/// limits aggregate damage, but without a line cap a leaked span stack can still
/// allocate and emit a multi-hundred-KiB event on every log call.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;

/// Rolls per window above which the writer complains on stderr.
const ROLL_ALERT_THRESHOLD: u32 = 10;
/// Window over which rolls are counted.
const ROLL_ALERT_WINDOW: Duration = Duration::from_secs(60);

struct RollState {
    file: Option<File>,
    /// Bytes written to the current file. Tracked in-process rather than by
    /// `stat()`-ing on every write — `make_writer` runs on every event.
    written: u64,
    rolls_in_window: u32,
    window_start: Instant,
    /// Suppresses repeated identical stderr complaints.
    alerted_this_window: bool,
}

struct Shared {
    state: Mutex<RollState>,
    path: PathBuf,
    max_bytes: u64,
    max_files: usize,
}

/// A [`MakeWriter`] adapter that discards bytes after a per-line limit.
///
/// The adapter reports discarded bytes as successfully written, then resumes at
/// the next newline. This keeps tracing operational during a runaway without
/// allowing any one formatted event to grow without bound.
#[derive(Clone)]
pub struct LineCappedMakeWriter<M> {
    inner: M,
    max_line_bytes: usize,
}

impl<M> LineCappedMakeWriter<M> {
    pub fn new(inner: M, max_line_bytes: usize) -> Self {
        Self {
            inner,
            max_line_bytes: max_line_bytes.max(1),
        }
    }
}

impl<'a, M> MakeWriter<'a> for LineCappedMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = LineCappedWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        LineCappedWriter::new(self.inner.make_writer(), self.max_line_bytes)
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        LineCappedWriter::new(self.inner.make_writer_for(meta), self.max_line_bytes)
    }
}

/// A writer that emits at most `max_line_bytes` bytes per newline-delimited line.
pub struct LineCappedWriter<W> {
    inner: W,
    max_line_bytes: usize,
    written_on_line: usize,
}

impl<W> LineCappedWriter<W> {
    fn new(inner: W, max_line_bytes: usize) -> Self {
        Self {
            inner,
            max_line_bytes: max_line_bytes.max(1),
            written_on_line: 0,
        }
    }
}

impl<W: Write> Write for LineCappedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Reserve one byte for the terminating newline. Long lines therefore
        // remain separate instead of being concatenated after truncation.
        let payload_limit = self.max_line_bytes.saturating_sub(1);
        let mut remaining = buf;

        while !remaining.is_empty() {
            let newline = remaining.iter().position(|byte| *byte == b'\n');
            let segment_len = newline.unwrap_or(remaining.len());
            let available = payload_limit.saturating_sub(self.written_on_line);
            let emit_len = segment_len.min(available);

            if emit_len > 0 {
                self.inner.write_all(&remaining[..emit_len])?;
                self.written_on_line += emit_len;
            }

            match newline {
                Some(index) => {
                    self.inner.write_all(b"\n")?;
                    self.written_on_line = 0;
                    remaining = &remaining[index + 1..];
                }
                None => break,
            }
        }

        // Discarded bytes are intentionally reported as consumed so the tracing
        // formatter does not retry them.
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A `MakeWriter` that appends to a file and rolls it once it exceeds a byte cap.
#[derive(Clone)]
pub struct SizeCappedWriter {
    shared: Arc<Shared>,
}

impl SizeCappedWriter {
    /// Open (or create) `path`, rolling at `max_bytes` and keeping `max_files`
    /// historical files alongside the live one.
    ///
    /// Total on-disk bytes are bounded by `max_bytes * (max_files + 1)`.
    pub fn new(path: impl AsRef<Path>, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        // Resume the byte count from whatever is already there, so a restart does
        // not get a fresh full-size budget on an existing file.
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);

        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(RollState {
                    file: Some(file),
                    written,
                    rolls_in_window: 0,
                    window_start: Instant::now(),
                    alerted_this_window: false,
                }),
                path,
                max_bytes: max_bytes.max(1),
                max_files,
            }),
        })
    }

    /// `base.log` → `base.log.1`, `base.log.1` → `base.log.2`, … dropping the oldest.
    fn rolled_path(&self, n: usize) -> PathBuf {
        let mut s = self.shared.path.clone().into_os_string();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    }

    fn roll(&self, st: &mut RollState) -> io::Result<()> {
        if let Some(f) = st.file.as_mut() {
            let _ = f.flush();
        }
        // Close before renaming — required on Windows, harmless elsewhere.
        st.file = None;

        if self.shared.max_files == 0 {
            // No history retained: just truncate in place.
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.shared.path)?;
            st.file = Some(file);
            st.written = 0;
            return Ok(());
        }

        // Drop the oldest, then shift everything down one slot.
        let oldest = self.rolled_path(self.shared.max_files);
        let _ = std::fs::remove_file(&oldest);
        for i in (1..self.shared.max_files).rev() {
            let from = self.rolled_path(i);
            if from.exists() {
                let _ = std::fs::rename(&from, self.rolled_path(i + 1));
            }
        }
        let _ = std::fs::rename(&self.shared.path, self.rolled_path(1));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.shared.path)?;
        st.file = Some(file);
        st.written = 0;

        self.note_roll(st);
        Ok(())
    }

    /// Count rolls per window and complain on stderr if the rate is pathological.
    /// A cap that silently absorbs a runaway is how a 159 GB/hr leak stays hidden.
    fn note_roll(&self, st: &mut RollState) {
        if st.window_start.elapsed() > ROLL_ALERT_WINDOW {
            st.window_start = Instant::now();
            st.rolls_in_window = 0;
            st.alerted_this_window = false;
        }
        st.rolls_in_window += 1;

        if st.rolls_in_window >= ROLL_ALERT_THRESHOLD && !st.alerted_this_window {
            st.alerted_this_window = true;
            // eprintln!, NOT tracing — this code *is* the tracing sink.
            eprintln!(
                "NEEDLE log writer: {} rolls in under {}s at {} bytes each ({}). \
                 Output is being discarded to stay within the cap — something is \
                 producing pathological log volume.",
                st.rolls_in_window,
                ROLL_ALERT_WINDOW.as_secs(),
                self.shared.max_bytes,
                self.shared.path.display()
            );
        }
    }
}

impl Write for SizeCappedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut st = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if st.written.saturating_add(buf.len() as u64) > self.shared.max_bytes {
            // A failed roll must not lose the line or abort the worker; fall through
            // and keep appending to the current file.
            if let Err(e) = self.roll(&mut st) {
                eprintln!("NEEDLE log writer: roll failed ({e}); continuing to append");
                if st.file.is_none() {
                    st.file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.shared.path)
                        .ok();
                }
            }
        }

        match st.file.as_mut() {
            Some(f) => {
                let n = f.write(buf)?;
                st.written = st.written.saturating_add(n as u64);
                Ok(n)
            }
            // Nowhere to write: swallow rather than kill the worker.
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut st = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match st.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SizeCappedWriter {
    type Writer = SizeCappedWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total_bytes(dir: &Path, stem: &str) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(stem))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    fn file_count(dir: &Path, stem: &str) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(stem))
            .count()
    }

    #[test]
    fn writes_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        let mut w = SizeCappedWriter::new(&path, 1024, 2).unwrap();
        w.write_all(b"hello\n").unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[test]
    fn rolls_once_the_cap_is_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        let mut w = SizeCappedWriter::new(&path, 32, 3).unwrap();

        for _ in 0..10 {
            w.write_all(b"0123456789ABCDEF\n").unwrap(); // 17 bytes
        }
        w.flush().unwrap();

        // The live file must be small; history must exist.
        assert!(
            std::fs::metadata(&path).unwrap().len() <= 32,
            "live file exceeded the cap"
        );
        let rolled = dir.path().join("w.log.1");
        assert!(rolled.exists(), "expected a rolled file at {rolled:?}");
    }

    /// The property that actually matters: total bytes on disk are bounded no
    /// matter how much is written. This is what hourly rotation could not give.
    #[test]
    fn total_bytes_stay_bounded_under_a_runaway() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        const MAX_BYTES: u64 = 512;
        const MAX_FILES: usize = 4;
        let mut w = SizeCappedWriter::new(&path, MAX_BYTES, MAX_FILES).unwrap();

        // Write far more than the cap — this is the 159 GB/hr scenario in miniature.
        let line = vec![b'x'; 200];
        for _ in 0..5_000 {
            w.write_all(&line).unwrap();
        }
        w.flush().unwrap();

        let ceiling = MAX_BYTES * (MAX_FILES as u64 + 1);
        let actual = total_bytes(dir.path(), "w.log");
        assert!(
            actual <= ceiling,
            "wrote 1,000,000 bytes; on-disk total {actual} exceeds ceiling {ceiling}"
        );
        assert!(
            file_count(dir.path(), "w.log") <= MAX_FILES + 1,
            "kept more files than max_files + 1"
        );
    }

    #[test]
    fn resumes_byte_count_from_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        std::fs::write(&path, vec![b'y'; 100]).unwrap();

        // Cap below what is already present: the first write must roll rather than
        // treat a restart as a fresh budget.
        let mut w = SizeCappedWriter::new(&path, 50, 2).unwrap();
        w.write_all(b"z").unwrap();
        w.flush().unwrap();

        assert!(
            dir.path().join("w.log.1").exists(),
            "did not roll on restart"
        );
        assert!(std::fs::metadata(&path).unwrap().len() <= 50);
    }

    #[test]
    fn max_files_zero_truncates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        let mut w = SizeCappedWriter::new(&path, 16, 0).unwrap();
        for _ in 0..20 {
            w.write_all(b"0123456789\n").unwrap();
        }
        w.flush().unwrap();

        assert_eq!(
            file_count(dir.path(), "w.log"),
            1,
            "kept history despite max_files=0"
        );
        assert!(std::fs::metadata(&path).unwrap().len() <= 16);
    }

    #[test]
    fn line_cap_discards_runaway_bytes_and_resumes_on_newline() {
        let mut writer = LineCappedWriter::new(Vec::new(), 16);
        writer
            .write_all(b"abcdefghijklmnopqrstuvwxyz\nok\n")
            .unwrap();

        assert_eq!(writer.inner, b"abcdefghijklmno\nok\n");
        assert!(writer
            .inner
            .split_inclusive(|byte| *byte == b'\n')
            .all(|line| line.len() <= 16));
    }

    #[test]
    fn line_cap_handles_a_line_split_across_writes() {
        let mut writer = LineCappedWriter::new(Vec::new(), 8);
        writer.write_all(b"12345").unwrap();
        writer.write_all(b"67890").unwrap();
        writer.write_all(b"\nnext\n").unwrap();

        assert_eq!(writer.inner, b"1234567\nnext\n");
        assert!(writer
            .inner
            .split_inclusive(|byte| *byte == b'\n')
            .all(|line| line.len() <= 8));
    }

    // ── line-cap boundary cases ─────────────────────────────────────────────
    //
    // The runaway and split-write tests above exercise lines well past the
    // cap; these pin the exact edge. The bound is on the emitted line *with*
    // its newline, so the payload limit is always `cap - 1`.

    /// A payload landing exactly on the limit must pass through untouched:
    /// `cap - 1` payload bytes plus the newline is precisely `cap`.
    #[test]
    fn line_exactly_at_the_cap_passes_through_untruncated() {
        const CAP: usize = 16;
        let mut writer = LineCappedWriter::new(Vec::new(), CAP);
        let payload = vec![b'a'; CAP - 1];
        writer.write_all(&payload).unwrap();
        writer.write_all(b"\n").unwrap();

        let mut expected = payload;
        expected.push(b'\n');
        assert_eq!(writer.inner, expected);
        assert_eq!(writer.inner.len(), CAP, "a cap-sized line is kept whole");
    }

    /// One byte over the cap loses exactly that byte: the emitted line is
    /// still `cap` bytes, and the discarded tail never leaks into the next
    /// line.
    #[test]
    fn line_one_byte_over_the_cap_truncates_by_exactly_one_byte() {
        const CAP: usize = 16;
        let mut writer = LineCappedWriter::new(Vec::new(), CAP);
        // 16 payload bytes: one past the `cap - 1` payload limit.
        writer.write_all(b"abcdefghijklmnop\n").unwrap();

        assert_eq!(writer.inner, b"abcdefghijklmno\n");
        assert_eq!(writer.inner.len(), CAP);
    }

    /// The limit is on the accumulated line, not on any single write: bytes
    /// may pile up to the payload limit across calls, and only the first byte
    /// past it is dropped.
    #[test]
    fn line_cap_boundary_is_on_the_accumulated_line_not_the_write() {
        const CAP: usize = 16;
        let mut writer = LineCappedWriter::new(Vec::new(), CAP);

        writer.write_all(b"0123456789").unwrap(); // 10 bytes
        writer.write_all(b"01234").unwrap(); // 15 — exactly the payload limit
        writer.write_all(b"5\n").unwrap(); // '5' is past it; '\n' terminates

        assert_eq!(writer.inner, b"012345678901234\n");
        assert_eq!(writer.inner.len(), CAP);
    }

    /// The degenerate cap of one still makes progress: every payload byte is
    /// dropped, the newline survives, and the writer never wedges.
    #[test]
    fn minimum_cap_of_one_emits_only_newlines() {
        let mut writer = LineCappedWriter::new(Vec::new(), 1);
        writer.write_all(b"suppressed\n").unwrap();
        assert_eq!(writer.inner, b"\n");
    }

    /// The production composition: the line cap wraps the rolling file
    /// writer at the `MakeWriter` boundary, so no path from a formatted
    /// tracing event to the file can exceed the cap — and a within-cap event
    /// still reaches it verbatim.
    #[test]
    fn make_writer_boundary_caps_lines_before_the_rolling_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capped.log");
        const CAP: usize = 128;

        let make_writer =
            LineCappedMakeWriter::new(SizeCappedWriter::new(&path, 64 * 1024, 2).unwrap(), CAP);
        let subscriber = tracing_subscriber::fmt()
            .with_writer(make_writer)
            .with_ansi(false)
            .without_time()
            .finish();

        let oversized = "y".repeat(CAP * 3);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(message = "oversized-boundary-probe", payload = %oversized);
            tracing::info!("compact-boundary-probe");
        });

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "one emitted line per event: {content:?}");
        // `line.len() < CAP` is the cap including the newline: the content on
        // disk never carries more than `CAP - 1` payload bytes.
        assert!(
            lines.iter().all(|line| line.len() < CAP),
            "every emitted line including its newline must be within the cap: {:?}",
            lines.iter().map(|l| l.len()).collect::<Vec<_>>()
        );
        assert_eq!(
            lines[0].len(),
            CAP - 1,
            "an oversized event degrades to exactly the payload limit"
        );
        assert!(
            lines[0].contains("oversized-boundary-probe"),
            "truncation keeps the leading part of the event: {:?}",
            lines[0]
        );
        assert!(
            lines[1].contains("compact-boundary-probe"),
            "a within-cap event passes through intact: {:?}",
            lines[1]
        );
    }
}
