# needle-la6l: Telemetry JSONL files created but empty

## Root Cause

`write_boot_event_direct_impl` in `src/telemetry/mod.rs` had no real implementation — it was a
stub that accepted parameters but never wrote to disk. The `_writer` parameter prefix confirmed
this. FileSink created the JSONL file on open (so `ls` showed the file), but nothing was ever
written into it.

## Fix

Replaced the stub with a real implementation that:

1. Constructs a `TelemetryEvent` for `worker.booting` in-memory.
2. Spawns a thread to open the file in append mode, serialize the event as JSONL, flush, and
   `sync_all()` to guarantee durability.
3. Uses `std::sync::mpsc::channel` with `recv_timeout(5s)` to join the thread without blocking
   indefinitely (guards against hung NFS mounts or slow filesystems).
4. Returns a descriptive error on timeout so callers can log a warning and continue.

The `_writer` mutex parameter was kept (renamed in signature to `_writer`) because the function
operates on a fresh file handle to avoid lock-ordering issues with the existing BufWriter.

## Verification

- Regression test `boot_event_written_to_file_on_telemetry_creation` added in `telemetry/mod.rs`.
- CI run `needle-ci-5lqd7` (2026-05-14T03:49Z) Succeeded.
