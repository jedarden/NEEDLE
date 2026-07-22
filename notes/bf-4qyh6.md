# bf-4qyh6: Load-Adaptive Launch Stagger Implementation - VERIFIED COMPLETE

## Task
P12.2 Load-adaptive launch stagger for --count=N batch launches

## Implementation Status: ✅ COMPLETE

This feature was fully implemented in commit `e5b32cb` by jedarden on 2026-07-22.

## What Was Implemented

### Core Functionality
- **Replaced** fixed 2-second `launch_stagger_seconds` sleep with load-aware delay
- **Added** `RateLimiter::load_adaptive_stagger()` function in `src/rate_limit/mod.rs`
- **Modified** `launch_workers()` in `src/cli/mod.rs` to use load-adaptive stagger

### Key Behavior
1. **Comfortable load**: Uses short default stagger (`base_stagger_secs`, default 2s)
2. **Saturated load**: Extends wait up to `max_wait_secs` (default 300s) with periodic rechecks
3. **Load recovery**: Proceeds immediately once load drops below threshold
4. **Bounded wait**: Never stalls indefinitely — caps at `max_wait_secs`
5. **Telemetry**: Emits `WorkerLaunchDeferred` events during extended waits

### Configuration Parameters (all with sensible defaults)
```yaml
worker:
  cpu_load_warn: 0.8                           # Normalized CPU threshold
  memory_free_warn_mb: 512                     # Available memory threshold (MB)
  adaptive_stagger_max_wait_secs: 300          # Max additional wait (5 minutes)
  adaptive_stagger_check_interval_secs: 5      # Recheck frequency
```

### Implementation Details

#### src/cli/mod.rs:647-661
```rust
// Stagger: load-adaptive delay before launching subsequent workers.
if seq > 0 && stagger_secs > 0 {
    let telemetry = Telemetry::new("cli-launch".to_string());
    
    RateLimiter::load_adaptive_stagger(
        config.worker.cpu_load_warn,
        config.worker.memory_free_warn_mb,
        stagger_secs,
        config.worker.adaptive_stagger_max_wait_secs,
        config.worker.adaptive_stagger_check_interval_secs,
        &telemetry,
    );
}
```

#### src/rate_limit/mod.rs:520-639
The `load_adaptive_stagger()` function:
- Checks CPU load (normalized by core count) and available memory
- Returns early if both are comfortable
- Enters extended wait loop if either is saturated
- Rechecks every `check_interval_secs` until recovery or `max_wait_secs` reached
- Emits telemetry events for observability

### Verification
- ✅ All rate_limit tests pass (21 tests, 0 failed)
- ✅ Config fields properly defined with defaults
- ✅ CLI integration complete
- ✅ Telemetry events implemented

## Design Rationale (from ADR-008)
Prevents batch launches from blindly pushing past the saturation threshold that would kill later-launched workers during the slow (~5s) worker_construction phase. Previously, a 10-worker batch with fixed 2s stagger could launch 8 workers before the first worker even finished construction, potentially saturating the system and causing OOM kills for workers 9-10.

With load-adaptive stagger:
- Workers 1-3 launch normally (system comfortable)
- Worker 4 detects saturation → waits with periodic rechecks
- Workers 5-10 are deferred until load drops or max_wait reached
- No workers are killed by OOM during construction

## Related Work
- P12.1: Resource gating before worker_construction (already complete)
- P12.3: Resource-adaptive Explore backoff (already complete)
- ADR-008: Fleet resource safety requirements
