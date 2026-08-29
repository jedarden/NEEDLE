# Binary Freshness Verification Guide

This guide provides comprehensive procedures for manually verifying that the binary freshness system works correctly in production and development environments.

## Overview

The binary freshness system ensures that workers automatically detect and rotate onto new binaries when they're deployed, without manual intervention. This enables the "fix-loop" workflow:

```
Developer commits fix → CI builds new binary → Binary deployed → Workers detect change → Workers rotate → New code runs automatically
```

## Verification Procedures

### 1. Verify Basic Binary Freshness Detection

**Purpose**: Confirm that workers can detect when their running binary differs from `needle-stable`.

**Procedure**:

1. Start a worker process:
   ```bash
   needle worker --config ~/.needle.yaml
   ```

2. Record the current worker PID:
   ```bash
   pgrep -f "needle worker" | head -1
   ```
   Let's call this `$WORKER_PID`.

3. Check the current binary hash:
   ```bash
   sha256sum $(needle home)/bin/needle-stable
   ```
   Record this as `$OLD_HASH`.

4. Trigger a binary update:
   ```bash
   cd /path/to/NEEDLE
   cargo build --release
   cp target/release/needle $(needle home)/bin/needle-stable
   ```

5. Compute the new binary hash:
   ```bash
   sha256sum $(needle home)/bin/needle-stable
   ```
   Record this as `$NEW_HASH`.

6. **Expected Result**: Within the freshness check interval (default 5 minutes), the worker should:
   - Detect the hash change
   - Log a message at INFO level: "new binary detected, initiating worker rotation"
   - Exit cleanly (exit code 0)
   - Supervisor should spawn a new worker process

7. Verify the new worker is running:
   ```bash
   ps -p $WORKER_PID
   # Should show "No such process" (old worker exited)
   
   pgrep -f "needle worker" | head -1
   # Should show a new PID (new worker spawned)
   ```

**Success Criteria**:
- Old worker process exits within 5 minutes of binary update
- New worker process is spawned automatically
- No manual intervention required

### 2. Verify Edge Cases

#### 2.1 Binary Missing Scenario

**Purpose**: Ensure graceful handling when `needle-stable` is missing.

**Procedure**:
1. Remove the stable binary:
   ```bash
   rm $(needle home)/bin/needle-stable
   ```

2. **Expected Result**: Worker should:
   - Log `WARN` level message: "monitored binary missing, skipping rotation check"
   - Continue running (not exit)
   - Check again on next interval

3. Restore the binary:
   ```bash
   cp target/release/needle $(needle home)/bin/needle-stable
   ```

**Success Criteria**: Worker continues running despite missing binary.

#### 2.2 Corrupt Binary Scenario

**Purpose**: Verify system handles corrupt or unreadable binaries.

**Procedure**:
1. Replace binary with corrupt data:
   ```bash
   dd if=/dev/urandom of=$(needle home)/bin/needle-stable bs=1024 count=10
   ```

2. **Expected Result**: Next freshness check should:
   - Log `WARN` level message: "binary freshness check failed"
   - Include error details
   - Continue running (graceful degradation)

3. Restore valid binary:
   ```bash
   cp target/release/needle $(needle home)/bin/needle-stable
   ```

**Success Criteria**: System degrades gracefully without crashes.

#### 2.3 Binary Unchanged (No Rotation)

**Purpose**: Verify workers don't rotate when binary is unchanged.

**Procedure**:
1. Touch the binary (update mtime without changing content):
   ```bash
   touch $(needle home)/bin/needle-stable
   ```

2. **Expected Result**: Worker should:
   - Not exit
   - Continue running normally
   - No rotation logs should appear

**Success Criteria**: No false positives - hash comparison prevents unnecessary rotation.

#### 2.4 Mid-Dispatch Check Blocked

**Purpose**: Ensure freshness checks don't interrupt active bead processing.

**Procedure**:
1. Start a worker processing a long-running bead (simulate with sleep):
   ```bash
   # Create a test bead that takes 2 minutes
   bead create --title "Long-running test" --priority 0 --body "Sleep for 120 seconds" --test-bead
   ```

2. While bead is processing, update the binary:
   ```bash
   cargo build --release
   cp target/release/needle $(needle home)/bin/needle-stable
   ```

3. **Expected Result**: Worker should:
   - Finish processing the current bead
   - Check freshness AFTER bead completion
   - Rotate before starting next bead

**Success Criteria**: Freshness checks respect bead processing boundaries.

### 3. Verify SEAM Supervisor Worker Cycling

**Purpose**: Confirm that `needle-supervisor-seam` actively monitors and cycles workers.

**Prerequisites**: Access to SEAM supervisor logs and metrics.

**Procedure**:

1. Check supervisor status:
   ```bash
   # On the SEAM host
   systemctl status needle-supervisor-seam
   # or
   ps aux | grep "needle-supervisor-seam"
   ```

2. Verify supervisor is running and has workers:
   ```bash
   # Get supervisor PID
   SUP_PID=$(pgrep -f "needle-supervisor-seam")
   
   # Check for child worker processes
   pgrep -P $SUP_PID
   # Should list worker PIDs
   ```

3. Trigger a binary update:
   ```bash
   # On the SEAM host
   cd /path/to/NEEDLE
   git pull
   cargo build --release
   sudo cp target/release/needle /usr/local/bin/needle-stable
   ```

4. Monitor supervisor logs for rotation events:
   ```bash
   # Tail supervisor logs (adjust log path as needed)
   tail -f /var/log/needle-supervisor-seam.log | grep -i "rotation\|freshness\|new binary"
   ```

5. **Expected Results**: Within 5 minutes, you should see:
   - `INFO`: "new binary detected, initiating worker rotation"
   - Workers receiving SIGTERM signals
   - Workers exiting cleanly
   - New workers spawning

6. Verify worker processes changed:
   ```bash
   # Before rotation
   ps aux | grep "needle worker" | awk '{print $2}' > /tmp/workers-before.txt
   
   # Wait for rotation (up to 5 minutes)
   sleep 300
   
   # After rotation
   ps aux | grep "needle worker" | awk '{print $2}' > /tmp/workers-after.txt
   
   # Compare PIDs
   diff /tmp/workers-before.txt /tmp/workers-after.txt
   # Should show different PIDs (rotation occurred)
   ```

**Success Criteria**:
- Supervisor detects binary change
- All workers rotate within 5 minutes
- No worker crashes during rotation
- Service availability maintained

### 4. Verify Fix-Loop End-to-End

**Purpose**: Demonstrate the complete fix-loop workflow from code commit to worker rotation.

**Procedure**:

1. **Make a trivial fix**:
   ```bash
   cd /path/to/NEEDLE
   echo "// Test fix" >> src/worker/mod.rs
   git add -A
   git commit -m "test: add binary freshness verification marker"
   git push
   ```

2. **Verify CI builds binary**:
   ```bash
   # Check iad-ci workflow
   kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
     get workflows -n argo-workflows \
     -l workflows.argoproj.io/workflow-template=needle-ci \
     --sort-by=.metadata.creationTimestamp | tail -5
   
   # Wait for build to complete
   # Verify new binary is available
   ```

3. **Deploy new binary**:
   ```bash
   # On SEAM host
   sudo cp target/release/needle /usr/local/bin/needle-stable
   ```

4. **Monitor rotation**:
   ```bash
   # Watch supervisor logs
   tail -f /var/log/needle-supervisor-seam.log | grep -i "rotation"
   
   # Watch for SIGTERM delivery
   tail -f /var/log/needle-supervisor-seam.log | grep -i "sigterm\|drain"
   ```

5. **Verify new workers are running**:
   ```bash
   # Check worker binary timestamps
   ls -l /proc/$(pgrep -f "needle worker" | head -1)/exe
   
   # Should point to the newly deployed needle-stable
   ```

6. **Verify fix is active**:
   ```bash
   # Check worker logs for the marker
   grep "binary freshness verification marker" /var/log/needle-worker*
   ```

**Success Criteria**:
- Entire loop completes without manual intervention
- Workers rotate within 5 minutes of deployment
- New code is running on all workers

### 5. Configuration Verification

**Purpose**: Confirm binary freshness configuration is correct.

**Procedure**:

1. Check freshness check interval:
   ```yaml
   # In ~/.needle.yaml or /etc/needle/config.yaml
   worker:
     freshness_check_interval_secs: 300  # 5 minutes default
   ```

2. Verify binary path configuration:
   ```yaml
   worker:
     worker_binary_path: /usr/local/bin/needle-stable  # Explicit path
     # OR leave unset to use auto-detection
   ```

3. Test configuration reload:
   ```bash
   # Change interval to 60 seconds for testing
   vi ~/.needle.yaml
   # Set freshness_check_interval_secs: 60
   
   # Reload supervisor
   systemctl reload needle-supervisor-seam
   # or
   kill -HUP $(pgrep -f "needle-supervisor-seam")
   ```

4. Verify new interval is active:
   ```bash
   # Check logs for configuration reload
   tail -f /var/log/needle-supervisor-seam.log | grep -i "reload\|config"
   ```

**Success Criteria**:
- Configuration values are loaded correctly
- Changes take effect on reload
- Workers respect configured intervals

## Troubleshooting

### Workers Not Rotating

**Symptoms**: Binary updated but workers continue running old version.

**Diagnosis**:
1. Check if supervisor is running:
   ```bash
   ps aux | grep supervisor
   ```

2. Check freshness check interval:
   ```bash
   grep freshness_check_interval ~/.needle.yaml
   ```

3. Check logs for errors:
   ```bash
   tail -100 /var/log/needle-supervisor-seam.log | grep -i "error\|fail"
   ```

**Solutions**:
- Ensure supervisor is running
- Reduce `freshness_check_interval_secs` temporarily for testing
- Verify binary path is correct
- Check file permissions on `needle-stable`

### Workers Crashing During Rotation

**Symptoms**: Workers exit with non-zero status during rotation.

**Diagnosis**:
1. Check worker exit codes:
   ```bash
   tail -100 /var/log/needle-worker* | grep -i "exit\|shutdown"
   ```

2. Check for SIGTERM handling issues:
   ```bash
   grep -i "sigterm\|signal" /var/log/needle-worker*
   ```

**Solutions**:
- Verify signal handlers are correctly installed
- Check for cleanup code that might be failing
- Ensure bead processing can be interrupted cleanly

### Supervisor Not Detecting Binary Changes

**Symptoms**: Supervisor logs no rotation events despite binary update.

**Diagnosis**:
1. Manually check binary hash:
   ```bash
   sha256sum /usr/local/bin/needle-stable
   ```

2. Check supervisor's cached hash:
   ```bash
   # This may require debug logging or introspection
   ```

3. Verify binary path:
   ```bash
   readlink -f /usr/local/bin/needle-stable
   ```

**Solutions**:
- Confirm binary path is correct
- Check file permissions
- Restart supervisor to clear cached state
- Verify SHA256 computation isn't failing

## Automated Verification Scripts

### Script: `verify-binary-freshness.sh`

```bash
#!/bin/bash
set -e

NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"
BINARY_PATH="$NEEDLE_HOME/bin/needle-stable"

echo "=== Binary Freshness Verification ==="
echo "NEEDLE_HOME: $NEEDLE_HOME"
echo "Binary path: $BINARY_PATH"
echo

# Check binary exists
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ FAIL: Binary not found at $BINARY_PATH"
    exit 1
fi
echo "✅ Binary exists"

# Check binary is executable
if [ ! -x "$BINARY_PATH" ]; then
    echo "❌ FAIL: Binary is not executable"
    exit 1
fi
echo "✅ Binary is executable"

# Get current hash
CURRENT_HASH=$(sha256sum "$BINARY_PATH" | awk '{print $1}')
echo "Current hash: $CURRENT_HASH"

# Check for running workers
WORKER_COUNT=$(pgrep -c -f "needle worker" || echo "0")
echo "Running workers: $WORKER_COUNT"

if [ "$WORKER_COUNT" -eq 0 ]; then
    echo "⚠️  WARNING: No workers currently running"
else
    echo "✅ Workers are running"
fi

# Check supervisor
if pgrep -f "needle-supervisor" > /dev/null; then
    echo "✅ Supervisor is running"
else
    echo "❌ FAIL: Supervisor is not running"
    exit 1
fi

echo
echo "=== Verification Complete ==="
echo "To trigger rotation, update the binary:"
echo "  cargo build --release && cp target/release/needle $BINARY_PATH"
echo "Workers should rotate within 5 minutes."
```

### Script: `monitor-rotation.sh`

```bash
#!/bin/bash
echo "=== Monitoring for worker rotation ==="
echo "Watching for process changes..."
echo

# Get initial workers
WORKERS_BEFORE=$(pgrep -f "needle worker" | tr '\n' ' ')
echo "Initial workers: $WORKERS_BEFORE"
echo

MAX_WAIT=300  # 5 minutes
ELAPSED=0

while [ $ELAPSED -lt $MAX_WAIT ]; do
    sleep 10
    ELAPSED=$((ELAPSED + 10))
    
    WORKERS_NOW=$(pgrep -f "needle worker" | tr '\n' ' ')
    
    if [ "$WORKERS_NOW" != "$WORKERS_BEFORE" ]; then
        echo "✅ Worker rotation detected at ${ELAPSED}s"
        echo "New workers: $WORKERS_NOW"
        exit 0
    fi
    
    echo "[$ELAPSED/${MAX_WAIT}s] No rotation yet..."
done

echo "❌ Timeout: No rotation detected within $MAX_WAIT seconds"
exit 1
```

## Conclusion

This verification guide provides comprehensive procedures for ensuring the binary freshness system works correctly. Regular verification of these procedures ensures that:

1. Workers automatically rotate onto new binaries
2. Edge cases are handled gracefully
3. The fix-loop workflow operates without manual intervention
4. Production deployments are reliable and safe

For issues or questions about binary freshness verification, consult the main NEEDLE documentation or raise an issue in the project repository.