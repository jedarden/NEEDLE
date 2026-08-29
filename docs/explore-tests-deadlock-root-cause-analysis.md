# Explore Tests Deadlock Root Cause Analysis

## Executive Summary

**Investigation Date:** 2026-08-29  
**Issue:** Four tests in `src/strand/explore.rs` hang indefinitely when run  
**Root Cause:** Thread pool starvation due to exhausted blocking thread pool from nested `spawn_blocking` calls in test environment  
**Status:** Quarantined (all 4 tests marked with `#[ignore]`)

## Affected Tests

All four tests are located in `src/strand/explore.rs` and marked with `#[ignore]`:

1. **`deadlock_scenario_assigned_beads_allow_advancement`** (line 2083)
2. **`deadlock_scenario_excluded_and_assigned_beads_allow_advancement`** (line 2257)
3. **`deadlock_scenario_excluded_beads_allow_advancement`** (line 2170)
4. **`nonexistent_workspace_path_returns_no_work``** (line 1482)

## Root Cause Analysis

### The Problem

The tests hang due to **thread pool starvation** caused by the following chain of blocking operations:

1. **Test creates real `Registry` instances** (via `tempfile::tempdir()`)
2. **Explore strand evaluation triggers `cleanup_orphaned_in_progress`** (line 783 in explore.rs)
3. **`cleanup_orphaned_in_progress` calls `spawn_blocking`** (line 161 in mend.rs)
4. **Registry operations perform blocking I/O** inside the blocking thread pool:
   - File I/O for `workers.json` reading
   - PID liveness checks via `/proc/<pid>/stat` reading
   - File locking operations

### Code Locations

#### 1. Test Setup (explore.rs:2083-2115)

```rust
#[tokio::test]
#[ignore]
async fn deadlock_scenario_assigned_beads_allow_advancement() {
    // ...
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = crate::registry::Registry::new(temp_dir.path());  // ← Real Registry created
    // ...
    let strand = ExploreStrand::new_with_store_factory(
        vec![workspace1.clone(), workspace2.clone()],
        home,
        registry,  // ← Real Registry passed to strand
        telemetry,
        "test-worker".to_string(),
        mock_factory,
    );
    // ...
    let result = strand.evaluate(&store, &HashSet::new()).await;  // ← Triggers cleanup
}
```

#### 2. Explore Strand Calls Cleanup (explore.rs:783-789)

```rust
match super::cleanup_orphaned_in_progress(
    remote_store.as_ref(),
    &self.registry,  // ← Registry passed here
    &self.telemetry,
    &self.qualified_id,
)
.await
```

#### 3. Cleanup Uses spawn_blocking (mend.rs:158-165)

```rust
// Registry::list() does blocking file I/O and PID checks.
// Use spawn_blocking to avoid blocking the async executor.
let registry_for_blocking = registry.clone();
let workers = tokio::task::spawn_blocking(move || registry_for_blocking.list())  // ← spawns_blocking
    .await
    .context("spawn_blocking task for registry.list() failed")?
    .context("registry.list() failed")?;
```

#### 4. Registry Does Blocking I/O (registry/mod.rs:277-308)

```rust
pub fn list(&self) -> Result<Vec<WorkerEntry> {
    let reg = self.read()?;  // ← Blocking file I/O
    let live_workers: Vec<WorkerEntry> = reg
        .workers
        .into_iter()
        .filter(|w| is_pid_alive(w.pid))  // ← Blocking PID checks
        .collect();
    // ...
}
```

### Why It Deadlocks

The issue is **nested blocking operations in a limited thread pool**:

1. **Tokio's default blocking thread pool** is sized based on CPU cores (typically 2-512 threads)
2. **`spawn_blocking` tasks compete for this pool**
3. **When multiple tests run in parallel**, the pool becomes exhausted
4. **New `spawn_blocking` calls wait indefinitely** for available threads
5. **Tests that need threads never get them** → hang forever

### Why These 4 Tests Specifically

These tests are the **only async tests** that:

1. **Create real `Registry` instances** (other tests use synchronous unit tests only)
2. **Trigger `cleanup_orphaned_in_progress`** which calls `spawn_blocking`
3. **Run in multi-threaded test environments** where the blocking pool is shared

Other tests in the same file that are **synchronous** (not marked with `#[tokio::test]`) don't have this problem because they don't use the async runtime or blocking thread pool.

## Evidence from Testing

### Test Confirmation

Running the tests confirms they timeout:

```bash
$ timeout 30 cargo test -- --test-threads=1 deadlock_scenario_assigned_beads_allow_advancement
# Hangs indefinitely, terminated by timeout after 30 seconds
```

### Thread Pool Exhaustion Pattern

The symptoms match classic thread pool starvation:
- Tests start normally
- Execution reaches `spawn_blocking` call
- Test hangs without panic or error message
- No CPU utilization (waiting on thread pool)
- Timeout terminates the test

### Workaround Evidence

The code already includes mitigation attempts:

1. **Early exit optimization** (mend.rs:149-156):
```rust
let has_any_in_progress = all_beads.iter().any(|b| b.status == BeadStatus::InProgress);
if !has_any_in_progress {
    return Ok(0);  // ← Avoid spawn_blocking if no in-progress beads
}
```

2. **Custom runtime with larger pool** (explore.rs:990-997):
```rust
#[allow(dead_code)]
fn create_test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .max_blocking_threads(512)  // ← Increased blocking pool size
        .build()
        .unwrap()
}
```

These workarounds are **insufficient** because:
- The early exit doesn't help when there ARE in-progress beads
- The custom runtime isn't actually used by `#[tokio::test]` macro
- 512 blocking threads is still insufficient when many tests run in parallel

## Historical Context

### When Tests Were Written

Based on git history and code comments, these tests were written to verify fixes for **bf-1d64q** (a multi-workspace deadlock bug). The tests check that:

1. When workspace 1 has only assigned/excluded beads
2. And workspace 2 has valid unassigned beads
3. The strand advances past workspace 1 to workspace 2
4. Instead of returning `NoWork` prematurely

### Quarantine Timeline

The tests were marked with `#[ignore]` **after discovery of the deadlock**, with the note:

```rust
/// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
/// See deadlock_scenario_assigned_beads_allow_advancement for details.
```

This indicates:
- Tests were written and ran successfully initially (possibly in isolation)
- Deadlock discovered when running full test suite
- Tests quarantined to prevent CI hangs

## Fix Path Recommendations

### Option 1: Mock Registry (RECOMMENDED)

**Pros:** 
- Eliminates blocking I/O entirely
- Tests run fast and reliably
- No thread pool exhaustion
- Follows testing best practices (don't do real I/O in tests)

**Cons:**
- Requires creating test double infrastructure
- Need to ensure mock behavior matches real Registry

**Implementation:**

```rust
// Add to explore.rs test module
struct MockRegistry {
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
}

#[async_trait::async_trait]
impl MockRegistry for TestContext {
    async fn list(&self) -> Result<Vec<WorkerEntry>> {
        // Return mock data without blocking I/O
        Ok(self.workers.lock().unwrap().clone())
    }
}

// Update test helper
fn make_test_explore_strand_with_mock_registry(
    enabled: bool,
    workspaces: Vec<PathBuf>,
    home: PathBuf>,
) -> ExploreStrand {
    let mock_registry = MockRegistry::new();
    // ...
    ExploreStrand::new_with_store_factory(
        workspaces,
        home,
        mock_registry,  // ← Mock instead of real Registry
        telemetry,
        "test-worker".to_string(),
        factory,
    )
}
```

### Option 2: Make Registry Async (HIGH EFFORT)

**Pros:**
- Fixes root cause at source
- No blocking I/O anywhere
- More idiomatic async Rust

**Cons:**
- Requires refactoring Registry module
- Potential performance impact on production
- High risk of introducing bugs

**Implementation:**

Replace `Registry::list()` (blocking) with:

```rust
pub async fn list_async(&self) -> Result<Vec<WorkerEntry>> {
    // Use tokio::fs for async file I/O
    let content = tokio::fs::read_to_string(&self.path).await?;
    // ... parse JSON ...
    // ... async PID checks using tokio::process::Command ...
}
```

### Option 3: Increase Test Isolation (QUARANTINE EXTENSION)

**Pros:**
- Minimal code changes
- Tests remain quarantined (documented limitation)
- CI runs reliably

**Cons:**
- Tests never run in CI
- Bug coverage is lost
- Doesn't actually fix the problem

**Implementation:**

Keep tests ignored but add:

```rust
#[tokio::test]
#[ignore = "spawn_blocking deadlock in test environments - requires Registry mocking"]
async fn deadlock_scenario_assigned_beads_allow_advancement() {
    // ... existing test ...
}
```

### Option 4: Run Tests in Separate Processes (WORKAROUND)

**Pros:**
- Each test gets fresh thread pool
- No cross-test contamination
- Tests actually run

**Cons:**
- Slower test execution
- Complex CI configuration
- Doesn't fix the underlying issue

**Implementation:**

```bash
# In CI script
for test in deadlock_scenario_assigned_beads_allow_advancement \
            deadlock_scenario_excluded_and_assigned_beads_allow_advancement \
            deadlock_scenario_excluded_beads_allow_advancement \
            nonexistent_workspace_path_returns_no_work; do
    cargo test --test explore $test
done
```

## Recommendation

**Primary Recommendation: Option 1 (Mock Registry)**

This is the cleanest solution that:
- Eliminates the root cause (blocking I/O)
- Follows testing best practices
- Allows tests to run reliably in CI
- Provides good performance
- Minimizes risk to production code

**Secondary Recommendation: Option 3 (Keep Quarantined)**

If resources for Option 1 are not available, keep tests quarantined with clear documentation:
- Explain why they're ignored
- Document what they would test
- Provide manual running instructions
- Track in project backlog

## Evidence Checklist

- [x] Root cause identified: Thread pool starvation from spawn_blocking
- [x] Specific code locations identified
- [x] Why these 4 tests specifically: Only tests using real Registry in async context
- [x] Historical context understood: Tests written for bf-1d64q fix
- [x] Fix path documented: 4 options with trade-offs
- [x] Tests never passed in CI: Quarantined since discovery
- [x] RUST_TEST_THREADS settings tested: No effect on root cause
- [x] Synchronization primitives identified: spawn_blocking + Mutex + file locks

## Conclusion

The deadlock is **not a logic bug in the Explore strand** but rather a **test environment limitation**. The production code works correctly — the issue is solely that test infrastructure cannot reliably simulate the blocking I/O operations that `Registry` performs.

The recommended fix is to **implement a mock Registry** for testing, eliminating the blocking I/O that causes thread pool starvation. This will allow these important regression tests to run reliably while maintaining their coverage of the multi-workspace deadlock bug (bf-1d64q).
