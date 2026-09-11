# Circuit Breaker Feature

> Automatic workspace build gating: when main is red, workers claim only fix-build beads.

**Type:** Improvement
**Status:** Draft
**Last updated:** 2026-09-01

---

## 1. Mission & North Star

**North Star:** Success is when workers automatically stop claiming non-fix-build beads in workspaces with failing main branch builds, preventing churn while allowing targeted fixes to restore the build to green.

**Background:** On 2026-08-28..30, NEEDLE continued claiming and processing beads in a workspace where main was red for 36 hours, resulting in ~225 unverified commits and a non-compiling main branch. Workers bypassed the failing build gate because the circuit was never opened. This feature implements that circuit breaker: before Pluck or Explore claims any bead, we check the workspace's build status and restrict claimable beads to those labeled `fix-build` (or `ci-red`) when the build is failing.

---

## 2. Non-Goals (Explicit Scope Boundaries)

- **No feature branch creation.** All work stays on main with no branches or extra clones per CLAUDE.md hard prohibitions and the operator's explicit decision (2026-08-30).
- **No worker-level isolation.** We are NOT implementing per-worker git worktrees or clones; the shared checkout model is intentional per ADR-015.
- **No cross-workspace coupling.** Circuit state is per-workspace; a failing build in repo X does NOT affect claiming in repo Y.
- **No manual intervention required.** The circuit opens and closes automatically based on build status; operators only intervene if the auto-created fix-build bead is insufficient.
- **No build status caching beyond 60 seconds.** Cache TTL is deliberately short to avoid stale state when builds are fixed; we do NOT implement longer TTLs or persistent caching.

---

## 3. Hard Requirements (Non-Negotiable)

- The system **MUST** check build status before claiming any bead via Pluck or Explore strands.
- The system **MUST** restrict claimable beads to those labeled `fix-build` or `ci-red` when the workspace's main branch is failing.
- The system **MUST** fall back to needle-ci workflow status when Forgejo commit status is unavailable.
- The system **MUST** emit a `strand.circuit_open` telemetry event when a workspace is found to be failing.
- The system **MUST** create a fix-build bead automatically via Splice when a gate fails on build and no open fix-build bead exists.
- The system **MUST** cache build status per workspace with a maximum 60-second TTL.
- The system **MUST** treat `Unknown` build status as "claim normally" (no circuit restriction).
- The system **MUST** expose circuit state via `needle status` command output.
- **Forbidden patterns:** The circuit breaker MUST NOT block the Splice strand from creating fix-build beads, and MUST NOT prevent workers from claiming beads in workspaces with Unknown build status.

### 3.1 Normative Language

The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY follow RFC 2119. When capitalized they are binding; in lowercase prose they are descriptive.

---

## 4. Glossary

- **Red workspace** — A workspace whose main branch has a failing build status (Forgejo commit status indicates failure, or needle-ci workflow phase failed).
- **Green workspace** — A workspace whose main branch has a passing build status.
- **Unknown workspace** — A workspace whose build status cannot be determined (no Forgejo status, no CI workflow, or query failed). Workers behave normally in unknown workspaces.
- **Circuit state** — The per-workspace evaluation of build status: Open (red, only fix-build beads claimable), Closed (green/unknown, normal claiming).
- **fix-build bead** — A bead labeled `fix-build` (or `ci-red`) that addresses a build failure. These are the only beads claimable when the circuit is open.
- **Forgejo status** — The commit status API result from Forgejo for the newest commit on `origin/main`.
- **CI fallback** — The needle-ci workflow phase for the repo's CI template, used when Forgejo status is unavailable.
- **Circuit breaker** — The overall mechanism that evaluates build status and restricts bead claiming when builds fail.

---

## 5. Acceptance Scenarios

### Scenario 1: Green Workspace — Normal Claiming (Happy Path)
- **Setup:** Workspace X has a passing main branch build (Forgejo status = success). Worker Y is ready to claim beads.
- **Action:** Worker Y runs the Pluck strand to select a bead from workspace X.
- **Expected:** BuildStatusChecker returns `Passing`. Pluck filters normally, and worker Y claims bead Z (no fix-build label).
- **Pass:** Bead Z is claimed and processed; no circuit_open telemetry event emitted; `needle status` shows workspace X as "circuit: closed".
- **Fail:** Worker Y is blocked from claiming non-fix-build beads despite green build; or circuit_open event emitted erroneously.

### Scenario 2: Red Workspace — Only Fix-Build Claimable (Error Path)
- **Setup:** Workspace X has a failing main branch build (Forgejo status = failure, commit SHA = abc123, first error line = "error: unused variable: x"). Worker Y is ready to claim beads.
- **Action:** Worker Y runs the Pluck strand to select a bead from workspace X.
- **Expected:** BuildStatusChecker returns `Failing`. Pluck skips all beads lacking `fix-build` or `ci-red` labels, emitting `strand.circuit_open` telemetry with workspace=X, sha=abc123.
- **Pass:** Worker Y claims no beads (or only an existing fix-build bead); circuit_open event includes workspace and failing SHA; `needle status` shows workspace X as "circuit: open".
- **Fail:** Worker Y claims a non-fix-build bead despite red build; no telemetry event emitted; or worker claims nothing when a fix-build bead exists.

### Scenario 3: Gate Failure — Auto-Created Fix-Build Bead (Recovery Path)
- **Setup:** Worker Y is processing bead Z in workspace X. The gate (definition-of-done --fast / cargo check) fails with build error. No open fix-build bead exists for X.
- **Action:** Worker Y runs the Splice strand after gate failure.
- **Expected:** Splice detects build failure, checks for existing open fix-build beads via BeadStore, finds none, and creates bead W with title="Fix build failure: <first error line>", priority=0, labels=[fix-build], commit SHA=<failing SHA>.
- **Pass:** Bead W is created successfully; bead W appears in the ready frontier; subsequent workers can claim W to fix the build.
- **Fail:** Splice fails to create fix-build bead; or creates duplicate fix-build beads for the same failure; or fix-build bead lacks required labels/priority.

### Scenario 4: Unknown Workspace — Normal Claiming (Degraded Path)
- **Setup:** Workspace X has no Forgejo status (new repo, or API unavailable) and no needle-ci workflow history.
- **Action:** Worker Y runs the Pluck strand to select a bead from workspace X.
- **Expected:** BuildStatusChecker returns `Unknown` (both fetch paths failed). Pluck behaves normally, claiming bead Z without circuit restriction.
- **Pass:** Bead Z is claimed and processed; no circuit restriction applied; `needle status` shows workspace X as "circuit: unknown".
- **Fail:** Worker Y is incorrectly blocked by circuit restriction despite Unknown status.

---

## 6. Architecture

### 6.1 Component Overview

**BuildStatusChecker** (`src/build_status.rs`)
- **Responsibility:** Query and cache build status per workspace. Primary: Forgejo commit status API. Fallback: needle-ci workflow phase via kubectl.
- **Dependencies:** tokio::process::Command (git rev-parse), HTTP client (Forgejo API), kubectl (CI workflows).
- **Consumers:** Pluck strand (before bead selection), Explore strand (before workspace discovery), Splice strand (for telemetry).

**Pluck Strand** (`src/strand/pluck.rs`)
- **Responsibility:** Select beads from the ready frontier, with new circuit breaker filtering step.
- **New integration:** After querying beads but before sorting, call BuildStatusChecker for workspace, filter out non-fix-build beads if Failing, emit telemetry.
- **Dependencies:** BeadStore, BuildStatusChecker, Telemetry.

**Explore Strand** (`src/strand/explore.rs`)
- **Responsibility:** Discover bead workspaces, with new circuit breaker check.
- **New integration:** Before returning a workspace, check build status; if Failing, emit telemetry and optionally filter beads (similar to Pluck).
- **Dependencies:** BuildStatusChecker, Telemetry.

**Splice Strand** (`src/strand/splice.rs`)
- **Responsibility:** Document worker failures and create fix-build beads when gates fail on build.
- **New integration:** Detect build failures in gate results, check BeadStore for existing open fix-build beads, create if missing with P0 priority and fix-build label.
- **Dependencies:** BeadStore, Telemetry, gate health detection.

**Config** (`src/config/mod.rs`)
- **New struct:** `CircuitBreakerConfig { enabled: bool, labels: Vec<String> }` in `PluckConfig`.
- **Default:** `enabled: true`, `labels: ["fix-build", "ci-red"]`.

**Telemetry** (`src/telemetry/mod.rs`)
- **New event type:** `strand.circuit_open` with payload: `{ workspace: String, status: "Failing", sha: String, source: "Forgejo"|"CI" }`.

**CLI** (`src/cli/mod.rs`)
- **New output:** `needle status` shows per-workspace circuit state in workspace summary section.

### 6.2 Data Flow

**Bead claiming with circuit breaker (Pluck strand):**

```text
1. Pluck queries BeadStore for ready beads in workspace X
2. Pluck calls BuildStatusChecker.check_status(workspace X)
   a. Check cache (workspace_key -> CachedStatus {status, expires_at})
   b. If cached and not expired, return status
   c. Else fetch fresh:
      - Run git rev-parse origin/main to get commit SHA
      - Call Forgejo API: GET /api/v1/repos/jedarden/REPO/statuses/<SHA>
      - If success, return Passing/Failing from status.state
      - If fail, fallback: kubectl get workflows -l workflows.argoproj.io/workflow-template=<template>
      - Update cache with 60s TTL
3. If status is Failing:
   a. Filter beads to only those with labels in ["fix-build", "ci-red"]
   b. Emit telemetry: strand.circuit_open { workspace, sha, status, source }
   c. Log warning: "Circuit OPEN for workspace X, only fix-build beads claimable"
4. If status is Passing or Unknown:
   a. Continue normal bead filtering and sorting
5. Return candidate list (possibly empty if circuit open and no fix-build beads exist)
```

**Fix-build bead creation (Splice strand after gate failure):**

```text
1. Gate runs definition-of-done --fast / cargo check on workspace X
2. Gate fails with build error: commit SHA=abc123, error="error: unused variable: x"
3. Splice detects build failure from gate result
4. Splice queries BeadStore for open beads with labels=["fix-build"] in workspace X
5. If none exist:
   a. Call BeadStore.create_bead with:
      - title="Fix build failure: error: unused variable: x"
      - priority=0
      - labels=["fix-build"]
      - body="Commit abc123 failed build. First error: error: unused variable: x\n\nThis bead auto-created by Splice circuit breaker."
   b. Emit telemetry: strand.splice_fix_build_created { workspace, bead_id, sha }
6. If exists:
   a. Log info: "Open fix-build bead already exists, skipping creation"
```

### 6.3 Concurrency / Execution Model

- **Single-writer cache:** BuildStatusChecker uses `Arc<RwLock<HashMap>>` for the cache. Multiple readers can check status concurrently; writes are exclusive but brief.
- **Async fetch:** Status fetching uses `async`/`await` via `tokio::process::Command` for git and HTTP client for Forgejo. This does NOT block the worker's main loop.
- **No distributed coordination:** Each worker maintains its own BuildStatusChecker cache. No cross-worker state sharing; circuit behavior emerges from independent workers reading the same Forgejo/CI ground truth.
- **Race tolerance:** If two workers both detect no fix-build bead and create one, `bead create` is NOT atomic. We accept duplicate fix-build beads (rare) as preferable to blocking on distributed locking. Duplicate beads can be closed manually or deduplicated by operators.

### 6.4 Technology Decisions (Why X Over Y)

**Forgejo API over git hooks:**
- Choice: Query Forgejo's commit status API directly.
- Alternative: Parse git hooks or CI job logs.
- Reason: Forgejo API is the single source of truth for commit status, already used by declarative-config. Git hooks would require per-repo setup and don't capture CI-run history.

**kubectl over CI API client:**
- Choice: Query needle-ci workflows via kubectl.
- Alternative: Call Argo Workflow API directly.
- Reason: kubectl is already installed on all workers and provides credential-free read access to iad-ci. Argo API would require token management. The credential-free pattern is consistent with CLAUDE.md Kubernetes access.

**In-memory cache over persistent cache:**
- Choice: Cache in process memory with 60s TTL.
- Alternative: Persist to disk, share across workers.
- Reason: Circuit state is short-lived (builds get fixed quickly). Persistent cache adds complexity without benefit. If the cache is stale, 60 seconds is acceptable latency for status propagation.

**Labels over priority:**
- Choice: Use `fix-build`/`ci-red` labels to identify claimable beads.
- Alternative: Use high priority (P0) to identify fix-build beads.
- Reason: Labels are semantic and already used for bead categorization. Priority would conflate "urgent" with "build fix", which may not always overlap. Labels allow explicit "this bead fixes the build" semantics.

---

## 7. Data Model

### 7.1 Core Entities

**BuildStatus** (enum, existing in `src/build_status.rs`)
```rust
pub enum BuildStatus {
    Passing,   // Build is passing
    Failing,   // Build is failing
    Unknown,   // Status unavailable
}
```

**CachedStatus** (struct, existing in `src/build_status.rs`)
```rust
struct CachedStatus {
    status: BuildStatus,
    expires_at: DateTime<Utc>,
}
```

**CircuitBreakerConfig** (new struct, in `src/config/mod.rs`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// Enable circuit breaker (default: true)
    #[serde(default = "CircuitBreakerConfig::default_enabled")]
    pub enabled: bool,

    /// Labels that identify fix-build beads (default: ["fix-build", "ci-red"])
    #[serde(default = "CircuitBreakerConfig::default_labels")]
    pub labels: Vec<String>,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            labels: Self::default_labels(),
        }
    }
}

impl CircuitBreakerConfig {
    fn default_enabled() -> bool { true }
    fn default_labels() -> Vec<String> {
        vec!["fix-build".to_string(), "ci-red".to_string()]
    }
}
```

**Telemetry event: strand.circuit_open** (new event type)
```json
{
  "timestamp": "2026-09-01T12:34:56Z",
  "event_type": "strand.circuit_open",
  "worker_id": "worker-alpha",
  "session_id": "a1b2c3d4",
  "sequence": 1234,
  "workspace": "/home/coding/NEEDLE",
  "data": {
    "status": "Failing",
    "sha": "abc123def456",
    "source": "Forgejo",
    "first_error": "error: unused variable: x"
  }
}
```

**Telemetry event: strand.splice_fix_build_created** (new event type)
```json
{
  "timestamp": "2026-09-01T12:35:00Z",
  "event_type": "strand.splice_fix_build_created",
  "worker_id": "worker-alpha",
  "session_id": "a1b2c3d4",
  "sequence": 1235,
  "workspace": "/home/coding/NEEDLE",
  "bead_id": "nb-abc123",
  "data": {
    "sha": "abc123def456",
    "title": "Fix build failure: error: unused variable: x",
    "priority": 0
  }
}
```

### 7.2 Source of Truth & Storage

- **Build status ground truth:** Forgejo commit status API (primary), needle-ci workflow phase (fallback).
- **Cache storage:** In-memory `HashMap` in `BuildStatusChecker`, scoped to worker process lifetime. Evicted on process restart.
- **Circuit state exposure:** `needle status` reads from BuildStatusChecker cache at invocation time (no persistent state).
- **Retention:** Cache entries expire after 60 seconds (configurable via `cache_ttl_secs`). No persistent storage.

---

## 8. Pre-Flight Safety

### 8.1 Edge Case Catalog

- **EC-01: Forgejo API unavailable.** Resolution: Fall back to needle-ci workflow status via kubectl. If both fail, return `Unknown` and allow normal claiming. Log warning but do not block workers.

- **EC-02: No main branch (repo has no commits, or main renamed).** Resolution: `git rev-parse origin/main` fails; catch error, return `Unknown`, log info "No main branch found, treating as Unknown".

- **EC-03: Commit SHA has no Forgejo status yet (CI still running).** Resolution: Forgejo API returns 404 or empty status array. Return `Unknown` (not `Failing`) so workers continue until CI completes and status is posted.

- **EC-04: kubectl not installed or iad-ci unreachable.** Resolution: CI fetch fails, fallback to `Unknown`. Log warning "CI unreachable, relying on Forgejo status only". Do not block on CI availability.

- **EC-05: Race condition — two workers create duplicate fix-build beads.** Resolution: Accept duplicates as rare but tolerable. The second bead creation will fail if `bead create` detects duplicate ID/title, or succeed with a new bead ID. Operators can close duplicates manually. No distributed locking.

- **EC-06: Circuit open but fix-build bead is already assigned to another worker.** Resolution: Pluck filters to fix-build beads but respects assignee constraints. If all fix-build beads are assigned, candidate list is empty. Worker waits for next cycle.

- **EC-07: Cache contains stale Passing status after build just failed.** Resolution: Cache TTL max 60 seconds. Workers may claim one bead in the window between failure and cache expiry. Acceptable latency for circuit propagation.

- **EC-08: Workspace path not a git repository.** Resolution: `git rev-parse` fails, return `Unknown`. Log warning "Not a git repository, cannot check build status". Do not crash.

- **EC-09: Splice cannot determine commit SHA from gate failure.** Resolution: Gate health detection must capture SHA from gate result. If SHA unavailable, create fix-build bead with SHA="unknown" in title.

- **EC-10: Fix-build bead creation fails (BeadStore error, network issue).** Resolution: Log error, emit telemetry `strand.splice_fix_build_failed`, continue worker loop. Next worker will retry creation. Do not block entire worker on creation failure.

### 8.2 Failure Modes & Recovery

| Failure | Detection | Recovery | Data Safety |
|---------|-----------|----------|-------------|
| Forgejo API timeout/down | HTTP request timeout or 5xx error | Fallback to CI status, then Unknown | No data loss; workers claim normally |
| CI workflow query fails | kubectl command fails non-zero | Return Unknown; log warning | No data loss; circuit relies on Forgejo |
| Cache corruption (RwLock poisoned) | RwLock read/write panic | Recreate cache on next check (graceful degradation) | In-memory cache only; safe to rebuild |
| Git command not found | Command::new("git") fails | Return Unknown; log error | No data loss; treat as unknown repo |
| BeadStore unavailable during fix-build creation | bead create command fails | Log error; retry next cycle | Bead creation is idempotent if retries check for existing bead |
| Malformed Forgejo response | JSON parse error | Return Unknown; log warning | No data loss; fallback to CI |

### 8.3 Invariants (Must Always Hold)

- **INV-1:** `Unknown` build status NEVER restricts bead claiming. Workers proceed normally.
- **INV-2:** Circuit breaker ONLY applies to workspace X's own beads; does NOT affect other workspaces.
- **INV-3:** Fix-build beads MUST have priority 0 (highest) to ensure they're claimed first once circuit opens.
- **INV-4:** Cache entries MUST NOT exceed configured TTL (60 seconds default). Expired entries are never returned.
- **INV-5:** Circuit state is derived from external ground truth (Forgejo/CI), never from persistent local state. A worker restart re-fetches status.

### 8.4 Rollback

**Rollback mechanism:** If the circuit breaker causes operational issues (e.g., false positives blocking legitimate work), disable via config:

```yaml
# .needle.yaml
strands:
  pluck:
    circuit_breaker:
      enabled: false
```

**State captured for rollback:**
- Build status cache is in-memory only; no persistent state to clean up.
- Fix-build beads created during operation remain as normal beads; close them manually if no longer needed.
- Telemetry events provide audit trail of circuit behavior; can be queried post-mortem.

**Rollback verification:** After setting `enabled: false`, workers should stop emitting `strand.circuit_open` events and claim beads normally regardless of build status. Verify via `needle status` showing "circuit: disabled" for all workspaces.

---

## 9. Phasing

### Phase 0: Walking Skeleton
**Goal:** Prove the wiring — one operation traverses every layer and returns a real result.

**Delivers:**
- Minimal `fetch_forgejo_status()` implementation: real HTTP call to Forgejo API, parses response, returns `BuildStatus::Passing` or `Failing`.
- Minimal `fetch_ci_status()` implementation: kubectl query for needle-ci workflows, parses phase, returns status.
- Pluck strand integration: calls `check_status()`, logs result, does not yet filter beads.
- One manual test: run `needle work` in a known workspace, observe build status check in logs.

**Does NOT include:** Error handling, caching, circuit filtering, fix-build bead creation, telemetry.

**Exit criteria:**
```bash
# Run worker in NEEDLE workspace (known passing build)
cargo run -- --work-once

# Logs show:
# [DEBUG] Build status for /home/coding/NEEDLE: Passing (from Forgejo)
# [INFO] Pluck strand complete, no bead claimed (test mode)
```

### Phase 1: Circuit Filtering & Telemetry
**Delivers:**
- Pluck strand circuit filtering: when `Failing`, filter beads to `fix-build` labels only.
- Telemetry event `strand.circuit_open` emitted when circuit opens.
- Explore strand integration: check build status before workspace discovery.
- Cache implementation: 60s TTL, in-memory HashMap.
- Config option: `strands.pluck.circuit_breaker.enabled` and `labels`.

**Completion criteria (all on the same commit):**
- Unit test: `test_circuit_filter_removes_non_fix_build_beads()` in `tests/strand_pluck_tests.rs`.
- Unit test: `test_cache_expires_after_ttl()` in `tests/build_status_tests.rs`.
- Integration test: `test_circuit_breaker_red_workspace_only_fix_build_claimable()` with mock Forgejo API returning failure.
- Manual check: Run `needle work`, verify `strand.circuit_open` event in telemetry JSONL.

**Does NOT include:** Splice auto-creation of fix-build beads, `needle status` output.

### Phase 2: Fix-Build Bead Auto-Creation
**Delivers:**
- Splice strand integration: detect build gate failures, check for existing fix-build beads, create if missing.
- Telemetry event `strand.splice_fix_build_created` when bead is created.
- Fix-build bead template: P0 priority, `fix-build` label, title with commit SHA and first error line.
- Gate health detection: extract commit SHA and error message from cargo check failures.

**Completion criteria (all on the same commit):**
- Unit test: `test_fix_build_bead_creation_on_gate_failure()` in `tests/strand_splice_tests.rs`.
- Integration test: `test_gate_failure_creates_fix_build_bead()` with simulated gate failure.
- Manual check: Trigger a real build failure in a test workspace, verify fix-build bead appears in ready frontier.

**Does NOT include:** `needle status` circuit state output (deferred to Phase 3).

### Phase 3: CLI Status Output & Docs
**Delivers:**
- `needle status` output: per-workspace circuit state (Open/Closed/Unknown) with failing SHA if open.
- Configuration documentation in `docs/configuration.md`: new `circuit_breaker` section.
- Telemetry documentation in `docs/agent-event-schema.md`: `strand.circuit_open` and `strand.splice_fix_build_created` events.
- ADR-019: Circuit Breaker Architecture Decision Record.

**Completion criteria (all on the same commit):**
- Manual check: `needle status` shows circuit state for each workspace in formatted output.
- Integration test: `test_needle_status_shows_circuit_state()` parses CLI output.
- Documentation review: `docs/configuration.md` includes example YAML and explanation of behavior.

### Phase 4: Comprehensive Testing & Hardening
**Delivers:**
- Edge case coverage: all EC-01 through EC-10 scenarios tested.
- Failure mode testing: Forgejo API timeout, kubectl failure, git command missing.
- Race condition testing: concurrent workers creating fix-build beads.
- Performance validation: cache hit rate, TTL accuracy, no memory leaks.
- Security review: Forgejo token handling, kubectl credential-free access validated.

**Completion criteria (all on the same commit):**
- All edge case unit tests pass (10+ new tests).
- Integration test with mock Forgejo API returning 500 errors, 404s, timeouts.
- Load test: 1000 consecutive status checks, cache remains bounded, no memory growth.
- Security checklist: no credentials in logs, kubectl uses credential-free endpoint, Forgejo token from env var only.

---

## 10. Testing Strategy & Quality Gates

### 10. Test Levels

- **Unit tests:**
  - `BuildStatus` enum methods (`is_failing`, `is_passing`, `is_unknown`)
  - `BuildStatusChecker` cache TTL logic, insertion, expiration
  - `CircuitBreakerConfig` defaults and deserialization
  - Bead filtering logic: `filter_by_circuit_labels()`

- **Integration tests:**
  - End-to-end: Worker with circuit breaker enabled against mock Forgejo API
  - Red workspace: only fix-build beads claimable, telemetry emitted
  - Green workspace: normal claiming, no circuit restriction
  - Unknown workspace: Forgejo and CI both fail, workers proceed normally
  - Splice fix-build creation: gate failure triggers bead creation

- **Scenario tests (maps to §5):**
  - Scenario 1 (Green): `test_green_workspace_normal_claiming()`
  - Scenario 2 (Red): `test_red_workspace_only_fix_build_claimable()`
  - Scenario 3 (Gate failure): `test_gate_failure_creates_fix_build_bead()`
  - Scenario 4 (Unknown): `test_unknown_workspace_normal_claiming()`

- **Property tests:**
  - Cache TTL: after N seconds, cached entry is expired
  - Label filtering: only beads with fix-build/ci-red pass when circuit open
  - Concurrent fix-build creation: no deadlock, at most one bead created

### 10.2 Quality Gates (Stop-Ship)

- **All acceptance scenarios pass:** Scenarios 1-4 must pass on the same commit.
- **No regression in existing Pluck behavior:** Tests for bead selection without circuit breaker must pass.
- **Telemetry events are valid JSON:** All emitted events parse against `docs/agent-event-schema.md`.
- **Config validation:** `strands.pluck.circuit_breaker.enabled=false` disables the feature entirely.
- **Documentation complete:** `docs/configuration.md` and `docs/agent-event-schema.md` updated.
- **Zero security findings:** No credentials in logs, no hardcoded tokens.
- **Performance budget:** 1000 status checks complete in <30 seconds, cache memory <10MB.

---

## 11. Security & Threat Model

| Threat | Vector | Mitigation | Test |
|--------|--------|------------|------|
| Forgejo API token leakage | Token logged in debug output or crash dump | Never log token; use environment variable only; sanitize error messages | Unit test: verify token not in telemetry events |
| Compromised Forgejo status | Attacker manipulates API to falsely report passing build | Verify API response signature (TLS); validate status state enum | Integration test: mock API returns invalid status, verify handled |
| kubectl credential confusion | Worker uses wrong kubeconfig with elevated privileges | Enforce credential-free endpoint `--server=http://traefik-iad-ci:8001` | Security test: ensure kubectl calls use read-only endpoint |
| Cache poisoning | Concurrent cache write corrupts state | Use `Arc<RwLock>` for exclusive writes; validate on read | Concurrency test: 100 workers share cache, verify no panics |
| Splice creates beads in wrong workspace | Gate failure from workspace X creates bead in workspace Y | Validate workspace path in Splice before bead creation | Unit test: splice failure in workspace A, verify bead created in A |
| Fix-build bead title injection | Attacker controls error message, injects malicious content | Escape or truncate error message in bead title; sanitize input | Security test: error message contains newlines, verify sanitized |

**Secrets:**
- **Forgejo token:** Source: environment variable `FORGEJO_TOKEN`. Storage: in-memory only during HTTP request lifetime. Never logged, never echoed. Rule: sanitize all error messages to remove token substring.

- **kubectl credentials:** No credentials required; use credential-free kubectl proxy endpoint per CLAUDE.md. Rule: verify kubectl command includes `--server=http://traefik-iad-ci:8001` in tests.

---

## 12. Performance Budgets

| Metric | Budget | Reference Condition | How Measured |
|--------|--------|---------------------|--------------|
| Build status check (cached hit) | <5ms p99 | Cache entry exists and not expired | Benchmark: 10,000 cache reads |
| Build status check (cache miss) | <500ms p99 | Forgejo API responds within 200ms, git rev-parse 50ms | Integration test: mock Forgejo API |
| Cache TTL accuracy | ±5 seconds | Configured 60s TTL | Unit test: insert entry, sleep 65s, verify expired |
| Cache memory growth | <10MB after 1000 workspaces | 1000 unique workspaces checked | Load test: 1000 status checks, measure RSS |
| Pluck strand overhead | <50ms additional latency | Circuit filtering adds to existing Pluck logic | Benchmark: Pluck with circuit enabled vs disabled |
| Concurrent workers | 10 workers sharing cache | No lock contention, no deadlocks | Concurrency test: 10 workers, 100 status checks each |

**Budget rationale:** Circuit status checks occur on every Pluck cycle (every few seconds). Cached reads must be near-instant to avoid impacting worker latency. Cache misses are rare (60s TTL), so 500ms is acceptable. Cache growth is bounded by workspace count, not check frequency.

---

## 13. Operations (Deploy, Migration, Monitoring)

### 13.1 Deployment & Configuration

**Installation:** Circuit breaker is compiled into the `needle` binary. Deploy via existing CI/CD (needle-ci workflow on iad-ci).

**Configuration:**
```yaml
# .needle.yaml (workspace-level override)
strands:
  pluck:
    circuit_breaker:
      enabled: true  # default: true
      labels: ["fix-build", "ci-red"]  # default: ["fix-build", "ci-red"]
```

**Environment variables:**
- `FORGEJO_TOKEN` — Required for Forgejo API access. Set via systemd service or shell env.
- `NEEDLE_CIRCUIT_BREAKER_CACHE_TTL` — Optional override for cache TTL in seconds (default: 60).

**CI invocation:** No special flags required. Feature is enabled by default; disable via config if needed.

### 13.2 Migration (if evolving an existing system)

**Data migration:** No persistent data to migrate. Cache is in-memory only; repopulated on worker start.

**Behavioral changes:**
- **Keep:** Workers continue claiming beads as before when builds are passing or unknown.
- **Drop:** Nothing removed; existing Pluck logic remains intact.
- **Reinterpret:** Beads lacking `fix-build` label are now skipped in failing workspaces (previously no restriction).

**Backward compatibility:**
- Config without `circuit_breaker` section uses defaults (enabled=true).
- Workers without `FORGEJO_TOKEN` set will fall back to CI status, or Unknown if both unavailable.
- Existing beads are unaffected; only future claiming behavior changes.

### 13.3 Monitoring & Health

**Health signals:**
- `strand.circuit_open` telemetry rate — High rate indicates many failing workspaces.
- `strand.splice_fix_build_created` telemetry — Confirms auto-creation is working.
- Build status cache hit rate — Should be >90% (most checks are cached).
- Build status check latency — p99 should be <500ms (cache misses).

**Doctor checks:**
- `bead doctor --check-circuit` — Verify Forgejo API reachability, kubectl access, cache state.
- `needle status --circuit-state` — Show per-workspace circuit state with failing SHAs.

**Alerting:**
- Circuit open for >1 hour in a workspace — May require operator intervention if auto-created fix-build bead is stuck.
- Zero circuit_open events but high gate failure rate — May indicate Splice integration is broken.

---

## 14. API / Interface Design

**CLI commands:**

```bash
# Check circuit state for all workspaces
needle status
# Output includes:
# Workspace: /home/coding/NEEDLE
#   Circuit: Open (Failing)
#   Failing SHA: abc123def456
#   Source: Forgejo

# Disable circuit breaker temporarily
needle work --config .needle.yaml --no-circuit-breaker
# (Requires adding --no-circuit-breaker flag to CLI args)
```

**Config interface (YAML):**

```yaml
strands:
  pluck:
    circuit_breaker:
      enabled: true
      labels:
        - fix-build
        - ci-red
```

**Telemetry interface (JSONL events):**

```json
// strand.circuit_open
{
  "timestamp": "2026-09-01T12:34:56Z",
  "event_type": "strand.circuit_open",
  "workspace": "/home/coding/NEEDLE",
  "data": {
    "status": "Failing",
    "sha": "abc123def456",
    "source": "Forgejo"
  }
}

// strand.splice_fix_build_created
{
  "timestamp": "2026-09-01T12:35:00Z",
  "event_type": "strand.splice_fix_build_created",
  "workspace": "/home/coding/NEEDLE",
  "bead_id": "nb-abc123",
  "data": {
    "sha": "abc123def456",
    "title": "Fix build failure: error: unused variable: x"
  }
}
```

**Programmatic interface (Rust):**

```rust
// BuildStatusChecker API
use crate::build_status::BuildStatusChecker;

let checker = BuildStatusChecker::with_default_ttl();
let status = checker.check_status(&workspace_path).await?;
match status {
    BuildStatus::Failing => {
        // Circuit is open, filter to fix-build beads only
    },
    BuildStatus::Passing | BuildStatus::Unknown => {
        // Normal claiming
    }
}

// Clear cache (for testing)
checker.clear_cache();
```

---

## 15. Risk Register & Plan B

| # | Risk | Likelihood | Impact | Mitigation |
|---|------|-----------|--------|------------|
| R1 | Forgejo API frequently unavailable or slow | Medium | Medium | Fallback to CI status; cache aggressively; log warnings |
| R2 | Circuit breaker false positive (Passing misreported as Failing) | Low | High | Validate Forgejo response schema; manually verify status before trusting; allow config override |
| R3 | Fix-build beads never get claimed (worker starvation) | Medium | High | P0 priority ensures they're first in queue; manual intervention if P0 isn't enough |
| R4 | Duplicate fix-build beads created by concurrent workers | Low | Low | Accept duplicates as rare; operators close manually |
| R5 | Cache TTL too long, workers claim during failing window | Low | Medium | Keep TTL at 60s; make configurable for operators |
| R6 | Splice integration broken, fix-build beads never created | Medium | High | Comprehensive integration tests; telemetry alerts if zero creations |
| R7 | Credential-free kubectl endpoint goes down | Low | Medium | Rely on Forgejo primary; Unknown fallback allows work to continue |
| R8 | Circuit blocker logic has bug, blocks all claiming | Low | Critical | Extensive unit tests; config disable flag for emergency rollback |

**Plan B:** If R8 (circuit bug blocks all claiming) occurs, the fallback is:

1. Immediate: Set `strands.pluck.circuit_breaker.enabled: false` in workspace `.needle.yaml`.
2. Short-term: Identify bug via telemetry (all beads skipped, circuit_open events for every workspace).
3. Long-term: Fix bug, redeploy, re-enable circuit breaker.

**Alternative approach if Plan B also fails:** Disable Pluck strand entirely and claim via Explore strand only (bypasses circuit logic). This is a last-resort emergency measure.

---

## 16. Open Questions

1. **Forgejo API authentication method.** Should we use a personal access token (PAT) or an OAuth app token? PAT is simpler for MVP but has per-user rate limits. OAuth app token requires setup but has higher limits. Owner: infra. Resolve by: Phase 0. Impact if wrong: Rate limiting may cause circuit breaker to fall back to CI more frequently.

2. **kubectl query format for needle-ci workflows.** Should we query by workflow template label or by workflow name prefix? Template label is more reliable but requires labels to be set correctly. Name prefix is fragile but works for existing workflows. Owner: platform. Resolve by: Phase 0. Impact if wrong: CI fallback may not work, causing Unknown status more often.

3. **Fix-build bead deduplication strategy.** Should we check for existing beads by title pattern (contains SHA) or by label only? Title matching is more precise but may miss variations. Label-only is simpler but may create duplicates for different failures. Owner: product. Resolve by: Phase 2. Impact if wrong: Duplicate beads or missing beads for multiple distinct failures.

4. **Circuit state persistence.** Should we cache circuit state to disk for faster restart, or always refetch on worker start? Disk cache is faster but risks stale state. Refetch is slower but always fresh. Owner: architecture. Resolve by: Phase 1. Impact if wrong: Either stale circuit state on restart (disk cache) or slower worker startup (refetch).

5. **Telemetry event schema for circuit_open.** Should we include the full error message from the build or just the first line? Full error is more diagnostic but may be large. First line is concise but may lose context. Owner: observability. Resolve by: Phase 1. Impact if wrong: Either bloated telemetry events or insufficient debugging info.

---

## 17. Revision History

| Date | Change | Author |
|------|--------|--------|
| 2026-09-01 | Initial draft generated from brief. | plan-author |
