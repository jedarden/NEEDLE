# Workspace Discovery Re-Run Decision Analysis

**Bead:** `bf-5lv0s`  
**Date:** 2026-07-20  
**Question:** Should workspace discovery re-run periodically, not just once at worker construction?

## Context

From `ExploreStrand::new()`'s doc comment:

> "The workspace list is captured at construction time and never re-read. If `workspaces` is empty, auto-discovers all dirs with `.beads/` under the configured `workspace_root`."

**Current behavior:** A worker's workspace list is fixed at construction time and never updated during its lifetime.

**Problem:** A long-lived worker will not see a repo created after it started without a restart.

## Operational Context Analysis

### Worker Lifecycle Patterns

Based on operational analysis and code review:

1. **Default idle_timeout: 120 seconds**
   - From `src/config/mod.rs`: default `idle_timeout` is 120 seconds (2 minutes)
   - Workers transition to EXHAUSTED when all strands return no work
   - Workers exit after idle_timeout is exceeded

2. **Workers are designed to cycle**
   - Workers are not meant to be permanent processes
   - Operational fleet lessons note: "Workers Exit When Done... Workers complete their workspace's beads and then exit via `idle_timeout`. This is normal behavior, not a crash."
   - Operators must periodically relaunch workers into workspaces with remaining work

3. **Hot-reload capability exists**
   - Workers support `--resume` for hot-reload into new binary
   - Canary and upgrade system can restart workers with updated code

### Discovery Frequency Analysis

**How often are repos created?**
- New repo creation is infrequent relative to worker cycles
- Example incident involved repos `commitgraph` and `twitterapi-proxy` that were missing from static list
- These repos existed but were invisible due to stale config, not recent creation

**How often do workers restart?**
- Default: Every 2 minutes when no work found
- In active fleet: Workers complete beads and eventually exhaust queues
- Manual restarts occur during operational interventions

## Trade-off Analysis

### Option 1: Periodic Re-Discovery

**Implementation:** Re-run `discover_workspaces()` every N cycles (e.g., every 100 cycles or on time interval)

**Pros:**
- ✅ Workers automatically detect new repos without manual intervention
- ✅ More responsive to infrastructure changes
- ✅ Reduces operational toil for repo additions

**Cons:**
- ❌ Adds complexity to worker loop state management
- ❌ Breaks the "static workspace list" invariant documented in ExploreStrand
- ❌ Could cause worker behavior inconsistency mid-operation
- ❌ Filesystem I/O on every cycle (or periodic cycles) adds overhead
- ❌ Adds configuration parameter (re-discovery interval) to maintain
- ❌ Tests become more complex (need to test periodic behavior)

### Option 2: External Signal-Based Re-Discovery

**Implementation:** Provide a signal mechanism (SIGHUP, filesystem marker, control socket) to trigger workspace re-discovery

**Pros:**
- ✅ On-demand re-discovery when operator knows a new repo was added
- ✅ No overhead during normal operation
- ✅ Preserves static workspace list invariant during normal operation
- ✅ Gives operators explicit control over when re-discovery occurs

**Cons:**
- ❌ Still requires manual action (send signal, touch file)
- ❌ More complex implementation than periodic re-discovery
- ❌ Signal handling complexity (async-signal safety)
- ❌ Need to document and teach operators about the mechanism

### Option 3: Accept Restart-Based Discovery (Status Quo)

**Implementation:** No code changes. Workers pick up new repos on next restart (natural or manual).

**Pros:**
- ✅ Simplest approach (no code changes needed)
- ✅ Preserves all existing invariants and design principles
- ✅ Workers already restart frequently (default 2-minute idle timeout)
- ✅ Matches the existing design philosophy: static config at boot
- ✅ No additional complexity or testing burden

**Cons:**
- ❌ Manual restart required if worker is long-lived and has new repos
- ❌ Operator must remember to restart workers after adding repos
- ❌ Delay between repo creation and worker awareness (up to idle_timeout)

## Recommendation

**Decision: Option 3 — Accept restart-based discovery as the standard pattern.**

### Rationale

1. **Workers are not designed to be long-lived processes**
   - The default 120-second idle_timeout means workers naturally cycle
   - Operational documentation reinforces this: "Workers Exit When Done... This is normal behavior, not a crash."
   - The "long-lived worker" scenario described in the bead is actually anti-pattern behavior

2. **The frequency of repo creation is low**
   - Adding new repos is an infrequent operational event
   - The cost of manual worker restart (seconds) is negligible compared to the benefit
   - Operational pattern: "Add repo → restart workers" is already natural workflow

3. **Preserves design simplicity**
   - The "static workspace list" invariant is well-documented and tested
   - Adding periodic re-discovery breaks this invariant for minimal benefit
   - Complex signal mechanisms add async-signal safety concerns

4. **Hot-reload already provides restart capability**
   - Workers already support graceful restart via `--resume`
   - Canary/upgrade system demonstrates restart is safe and routine
   - SIGHUP support already exists for graceful shutdown

5. **No additional operational burden in practice**
   - With default idle_timeout of 120 seconds, workers auto-restart within 2 minutes of exhaustion
   - Manual restart is trivial: `needle stop && needle run`
   - Fleet operators already monitor and manage worker lifecycle

### Implementation Guidance

**No code changes required.** Update documentation to clarify the expected pattern:

1. **Document in ExploreStrand doc comment:**
   ```rust
   //! Workspace discovery runs once at worker construction and is static for the worker's lifetime.
   //! To pick up newly-created repos, restart the worker. Workers are designed to cycle naturally
   //! via idle_timeout (default: 120 seconds), so new repos are typically visible within minutes
   //! without manual intervention.
   ```

2. **Add operational note to CLAUDE.md or README:**
   ```markdown
   ### Adding New Repos to the Fleet
   
   When creating a new repo with `.beads/`, roaming workers will automatically discover it
   on their next restart. With the default idle_timeout (120 seconds), exhausted workers
   restart automatically within 2 minutes. For immediate pickup, manually restart workers:
   
   ```bash
   needle stop --all
   needle run --agent claude --count 10
   ```
   ```

3. **Consider telemetry enhancement (optional):**
   - Emit `workspace.discovery.count` event at worker construction with discovered workspace count
   - Helps operators verify discovery is working correctly
   - No re-discovery needed — just better visibility

### When to Reconsider This Decision

Revisit periodic re-discovery if any of these conditions emerge:

1. **Repo creation becomes high-frequency**
   - If repos are being created multiple times per day
   - If repo creation becomes automated/programmatic

2. **Workers become genuinely long-lived**
   - If idle_timeout defaults are increased to hours/days
   - If operational patterns shift to "one worker runs for weeks"

3. **Operational feedback indicates manual restart burden**
   - If operators frequently complain about needing to restart workers
   - If incidents occur due to forgotten restarts after repo additions

4. **Signal-based infrastructure matures**
   - If NEEDLE gains a general control/management interface
   - If SIGHUP-based config reload is implemented for other reasons

## Alternative: Hybrid Approach (If Periodic Re-Discovery is Strongly Desired)

If periodic re-discovery is implemented despite this recommendation:

```rust
// In ExploreConfig
#[serde(default = "ExploreConfig::default_rediscovery_interval")]
pub rediscovery_interval_cycles: u64,  // Re-discover every N cycles, 0 = never

// In ExploreStrand::evaluate()
if self.cycle_count % self.rediscovery_interval_cycles == 0 {
    let updated = Self::discover_workspaces(&config.workspace_root);
    if updated != self.workspaces {
        tracing::info!("workspace list changed from {:?} to {:?}", self.workspaces, updated);
        self.workspaces = updated;
    }
}
```

**Design notes for hybrid approach:**
- Make it configurable (interval can be increased to effectively disable)
- Emit telemetry when workspace list changes
- Log clearly when re-discovery occurs
- Add tests for periodic re-discovery behavior
- Document the performance implications

## References

- `src/strand/explore.rs`: ExploreStrand::new() doc comment and discover_workspaces()
- `docs/adr/004-recursive-workspace-discovery-default.md`: ADR that established discovery as default
- `docs/plan/plan.md` Phase 8.3: Original reference to this open question
- `src/config/mod.rs`: idle_timeout default (120 seconds)
- `docs/notes/operational-fleet-lessons.md`: Worker lifecycle patterns
