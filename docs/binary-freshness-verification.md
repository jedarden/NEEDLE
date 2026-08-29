# Binary Freshness Verification Guide

This guide explains how to verify that the binary freshness system is working correctly in your NEEDLE deployment.

## Overview

The binary freshness system ensures that workers automatically detect and rotate onto new binaries when fixes are deployed. This eliminates the need for manual rolling restarts or worker redeployments.

## Architecture

### Components

1. **Worker Freshness Check** (`src/worker/mod.rs`)
   - Each worker periodically checks if its running binary matches `needle-stable`
   - Runs at `worker.freshness_check_interval_secs` (default: 60s)
   - Compares SHA256 hashes of current vs. stable binary
   - Exits cleanly with exit code 0 if mismatch detected

2. **Supervisor Freshness Monitor** (`src/supervisor/mod.rs`)
   - Supervisor monitors `needle-stable` for changes
   - Uses `BinaryFreshnessChecker` with configurable interval
   - Detects `NewBinary`, `BinaryMissing`, and `CheckFailed` states
   - Emits telemetry events for each state transition

3. **Build Metadata** (`src/build_metadata.rs`)
   - Embeds version, commit SHA, and build timestamp in binary
   - Read via `BuildMetadata::current()` and `BuildMetadata::from_binary()`
   - Provides human-readable version strings for logs and telemetry

## Manual Verification Procedure

### Prerequisites

- SSH access to the supervisor server (rs-manager or ex44)
- `kubectl` configured for the appropriate cluster
- `needle` binary installed
- Supervisor running (either systemd service or Kubernetes deployment)

### Step 1: Verify Supervisor is Running

```bash
# Check supervisor process (systemd deployment)
ps aux | grep needle-supervisor

# Or check Kubernetes deployment (if deployed in cluster)
kubectl get pods -n <namespace> -l app=needle-supervisor

# Check supervisor heartbeat file
ls -la ~/.needle/supervisor.heartbeat
stat ~/.needle/supervisor.heartbeat
```

**Expected:** Supervisor process/deployment is running and heartbeat file is recent (modified within last 60 seconds).

### Step 2: Check Current Binary Metadata

```bash
# Check version of currently running needle
needle --version

# Check needle-stable binary metadata
~/.needle/bin/needle-stable --version

# Compare commit SHAs
needle --version | grep -oE 'commit [a-f0-9]+'
~/.needle/bin/needle-stable --version | grep -oE 'commit [a-f0-9]+'
```

**Expected:** Both binaries show the same version and commit SHA (if workers are up-to-date).

### Step 3: Deploy a Test Binary Change

```bash
# Backup current stable binary
cp ~/.needle/bin/needle-stable ~/.needle/bin/needle-stable.backup

# Create a test binary change (e.g., rebuild with a comment)
# In production, this would be a real CI/CD deployment
cargo build --release
cp target/release/needle ~/.needle/bin/needle-stable-test

# Simulate deployment by replacing stable binary
mv ~/.needle/bin/needle-stable-test ~/.needle/bin/needle-stable
```

### Step 4: Monitor Supervisor Logs

```bash
# Tail supervisor logs (systemd)
journalctl -u needle-supervisor -f

# Or Kubernetes logs
kubectl logs -f <supervisor-pod> -n <namespace>

# Look for these log messages:
# - "new binary detected, initiating worker rotation"
# - "monitored binary missing, skipping rotation check"
# - "binary freshness check failed"
```

**Expected:** Within 60 seconds (or configured `supervisor.freshness_check_interval_secs`), you should see a log message indicating the new binary was detected.

### Step 5: Monitor Worker Telemetry

```bash
# Check recent worker telemetry events
grep "worker.binary_freshness_exit" ~/.needle/logs/needle-supervisor.events.jsonl | tail -5

# Or query OpenTelemetry if configured
# Look for events with:
# - event_kind: "worker.binary_freshness_exit"
# - old_hash: <previous binary hash>
# - new_hash: <new binary hash>
```

**Expected:** Worker exits with `worker.binary_freshness_exit` event containing the old and new binary hashes.

### Step 6: Verify Worker Rotation

```bash
# Check worker registry
ls -la ~/.needle/registry/

# Check for new worker processes
ps aux | grep 'needle.*worker'

# Verify new binary is being used
# Get PID of a worker and check its executable
WORKER_PID=$(pgrep -f 'needle.*worker' | head -1)
ls -l /proc/$WORKER_PID/exe
readlink /proc/$WORKER_PID/exe

# Check worker version
~/.needle/bin/needle-stable --version
```

**Expected:** Workers are running the new binary (`/proc/$PID/exe` points to the new needle-stable).

### Step 7: Restore Original Binary (Cleanup)

```bash
# Restore original stable binary
mv ~/.needle/bin/needle-stable.backup ~/.needle/bin/needle-stable

# Verify supervisor detects this change too
journalctl -u needle-supervisor -f | grep -i "new binary"
```

## Verifying needle-supervisor-seam

The SEAM supervisor deployment should automatically cycle workers when new binaries are deployed.

### Check SEAM Supervisor Status

```bash
# Get SEAM supervisor pod
kubectl get pods -n seam -l app=needle-supervisor-seam

# Check logs
kubectl logs -f <pod-name> -n seam

# Describe pod to see restarts
kubectl describe pod <pod-name> -n seam
```

**Expected:** Pod is running and logs show periodic freshness checks without errors.

### Deploy Test Change to SEAM

```bash
# Build new binary
cd /home/coding/NEEDLE
cargo build --release

# Copy to SEAM's stable binary location
# (This may vary based on deployment - adjust path accordingly)
kubectl cp target/release/needle <seam-supervisor-pod>:/app/.needle/bin/needle-stable -n seam

# Monitor SEAM supervisor logs
kubectl logs -f <seam-supervisor-pod> -n seam | grep -i "binary"
```

**Expected:** SEAM supervisor detects the new binary and initiates worker rotation.

## Troubleshooting

### Workers Not Rotating

**Symptom:** Workers continue running old binary after new deployment.

**Possible Causes:**

1. **Stale binary not detected**
   - Check freshness check interval: `grep freshness_check_interval ~/.needle/config.yaml`
   - Verify needle-stable path is correct: `ls -la ~/.needle/bin/needle-stable`
   - Check file permissions: `ls -l ~/.needle/bin/needle-stable`

2. **Supervisor not running**
   - Verify supervisor process: `ps aux | grep needle-supervisor`
   - Check systemd status: `systemctl status needle-supervisor`
   - Review logs: `journalctl -u needle-supervisor -n 50`

3. **Workers ignoring freshness check**
   - Check worker config: `grep worker.freshness_check ~/.needle/config.yaml`
   - Review worker logs: `grep freshness ~/.needle/logs/worker-*.log`
   - Verify telemetry events: `grep worker.binary_freshness ~/.needle/events.jsonl`

### Binary Hash Mismatch

**Symptom:** Logs show hash mismatch but workers don't exit.

**Possible Causes:**

1. **Check interval too long**
   - Reduce `worker.freshness_check_interval_secs` in config
   - Workers check only at the configured interval (default: 60s)

2. **Worker stuck in long-running dispatch**
   - Workers complete current dispatch before checking freshness
   - Check agent timeout: `grep timeout ~/.needle/config.yaml`
   - Review active agent processes: `ps aux | grep 'claude\|aider\|opencode'`

### Deleted Binary Detection

**Symptom:** Worker binary shows " (deleted)" in `/proc/self/exe`.

**Explanation:** This occurs when the running binary is replaced (e.g., `mv needle needle.old`).

**Expected Behavior:** Worker should detect this and hot-reload to `needle-stable`.

**Verification:**

```bash
# Check if current binary is deleted
readlink /proc/$WORKER_PID/exe | grep "deleted"

# Worker should exit and reload to stable
tail -f ~/.needle/logs/worker-*.log | grep "hot.reload"
```

## Testing the Fix-Loop

The following test demonstrates the complete fix-loop:

```
┌─────────────────────────────────────────────────────────────┐
│                    FIX LANDS                                 │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ 1. Developer commits fix                             │   │
│  │ 2. CI builds new needle binary                       │   │
│  │ 3. New binary deployed to ~/.needle/bin/needle-stable│   │
│  └──────────────────────────────────────────────────────┘   │
│                         │                                     │
│                         ▼                                     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Supervisor detects new binary (within check interval) │   │
│  │ - Emits FreshnessCheck::NewBinary                    │   │
│  │ - Logs new hash                                       │   │
│  └──────────────────────────────────────────────────────┘   │
│                         │                                     │
│                         ▼                                     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Workers continue current dispatch                     │   │
│  │ - Finish processing current bead                      │   │
│  │ - Emit outcome telemetry                             │   │
│  └──────────────────────────────────────────────────────┘   │
│                         │                                     │
│                         ▼                                     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Worker check_hot_reload() on next iteration          │   │
│  │ - Compares current binary hash to stable             │   │
│  │ - Detects mismatch                                   │   │
│  │ - Exits cleanly (exit code 0)                        │   │
│  │ - Emits worker.binary_freshness_exit telemetry      │   │
│  └──────────────────────────────────────────────────────┘   │
│                         │                                     │
│                         ▼                                     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Supervisor spawns new worker                          │   │
│  │ - Launches needle-stable (new binary)                │   │
│  │ - Worker runs with new code                           │   │
│  │ - Future beads use new fix                           │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## Automated Tests

Run the integration tests to verify the system:

```bash
# Run binary freshness integration tests
cargo test binary_freshness_integration

# Run long-lived worker rotation test
cargo test long_lived_worker_binary_rotation

# Run deleted binary hot-reload test
cargo test verify_deleted_binary_hot_reload

# Run all binary freshness tests
cargo test freshness
```

## Configuration Reference

### Worker Config

```yaml
worker:
  # Path to worker binary (optional, defaults to current exe)
  worker_binary_path: /path/to/needle

  # How often workers check binary freshness (seconds)
  freshness_check_interval_secs: 60

  # Action to take when stale binary detected
  idle_action: exit  # or "continue" for testing
```

### Supervisor Config

```yaml
supervisor:
  # Maximum concurrent workers
  max_workers: 4

  # How often supervisor checks for new binary (seconds)
  freshness_check_interval_secs: 60

  # Path to worker binary (optional)
  worker_binary_path: /path/to/needle
```

## Summary

The binary freshness system provides:

- ✅ **Zero-downtime upgrades**: Workers rotate onto new binaries without manual intervention
- ✅ **Automatic propagation**: Fixes land → binary builds → workers adopt new code
- ✅ **Clean exits**: Workers complete current dispatch before exiting
- ✅ **Comprehensive telemetry**: All state changes logged for monitoring
- ✅ **Graceful degradation**: Missing/corrupt binaries handled safely

For issues or questions, refer to:
- Integration tests: `tests/binary_freshness_integration.rs`
- Worker implementation: `src/worker/mod.rs` (check_hot_reload)
- Supervisor implementation: `src/supervisor/mod.rs` (BinaryFreshnessChecker)
