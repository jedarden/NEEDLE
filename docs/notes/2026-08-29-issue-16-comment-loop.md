# GitHub Issue #16 Comment Loop Incident

## Timeline

- **2026-08-28T21:43Z**: Bead `needle-0fbf5145` ("Post comment to GitHub issue #16") first closed
- **2026-08-28T21:43Z - 2026-08-29T10:08Z**: Bead cycled closed → reopened 14 times
- **Each cycle**: ~30 seconds between close and system-initiated reopen
- **Final state**: 18 total comments posted to https://github.com/jedarden/NEEDLE/issues/16 (9 byte-identical)
- **Resolution**: Bead manually set to `deferred` to stop the loop

## Root Cause

The outcome handler in `src/outcome/mod.rs` reset the failure count **before** running shipped-work verification. Specifically:

1. Agent exits 0, verification gates pass → `handle_success()` called
2. Bead is closed (agent called `bf close`)
3. When `worker.enforce_shipped_work` was disabled, the failure count was reset unconditionally on line 495
4. When `worker.enforce_shipped_work` was enabled but shipped-work check errored, no reset occurred but the bead was still marked as completed
5. Bead marked as completed, then reopened by `handle_gate_failure()` for shipped-work failure
6. Next cycle: count already reset to 0, loop repeats forever

The failure count meant to quarantine repeat offenders was zeroed on every pass, so the threshold was never reached.

## The Fix (commit c3921e09)

Moved the `reset_failure_count()` call to happen ONLY after shipped-work verification **passes**:

### Before (BROKEN):
```rust
if self.config.worker.enforce_shipped_work {
    match verify_shipped_work(&current, &bead.workspace, store).await {
        Ok(crate::validation::GateResult::Fail(reason)) => {
            // Reopen and release, incrementing failure count
            return self.handle_gate_failure(store, bead, &report).await;
        }
        Ok(crate::validation::GateResult::Pass) => {
            // Reset count
            let _ = self.reset_failure_count(store, bead).await;
        }
        Err(e) => {
            // Error case - no reset, but also no quarantine
        }
    }
} else {
    // Shipped-work enforcement disabled - ALWAYS reset
    let _ = self.reset_failure_count(store, bead).await;  // BUG!
}
```

### After (FIXED):
```rust
if self.config.worker.enforce_shipped_work {
    match verify_shipped_work(&current, &bead.workspace, store).await {
        Ok(crate::validation::GateResult::Fail(reason)) => {
            // Reopen and release, incrementing failure count
            return self.handle_gate_failure(store, bead, &report).await;
        }
        Ok(crate::validation::GateResult::Pass) => {
            // Reset count - ONLY after verification passes
            let _ = self.reset_failure_count(store, bead).await;
        }
        Err(e) => {
            // Error case - do NOT reset, let failure count accumulate
            tracing::warn!(...);
        }
    }
} else {
    // Shipped-work enforcement disabled - DO NOT reset
    tracing::debug!(...);
}
```

## Operator Rule: Beads with External Side Effects

Beads that perform external side effects (GitHub comments, API calls, provisioning steps) MUST verify the effect before closing:

1. **For deliverable:external beads**: The shipped-work gate requires a `notes` update with an `evidence:` line containing a URL or identifier
2. **For any bead with external side effects**: Record evidence in the bead notes that the effect occurred:
   ```bash
   bead update <id> --notes "evidence: https://github.com/user/repo/issues/16#comment-123"
   bead close <id> --reason "posted comment as requested"
   ```
3. **Verification before repeat**: Before performing a repeatable external action, check if it was already done:
   - For comments: check the issue/PR for existing comments from your user
   - For API calls: check the resource state
   - For provisioning: check if the resource already exists

Without this check, a bead that closes without evidence will be quarantined after `quarantine_after_failures` attempts (default: 5), but multiple workers may each post the same comment before quarantine takes effect.

## Related Beads

- `needle-b39fe1b6`: This fix (moved reset after shipped-work verification)
- `needle-9037530d`: FalseCloseDetected telemetry event (blocked by this bead)

## Impact

This fix prevents indefinite loops where beads close without shipped work, posting duplicate external side effects (comments, API calls) on every cycle. The failure count now accumulates correctly and triggers quarantine at the configured threshold.
