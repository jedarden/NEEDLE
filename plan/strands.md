# Strand Waterfall

Strands are NEEDLE's strategy for finding work. They are evaluated in strict sequence — the first strand that yields actionable work wins. When a strand returns `NoWork`, the worker falls through to the next.

The waterfall is the answer to "what does a worker do when it has no beads?" It is not a priority system for beads (that's handled by deterministic ordering within each strand). It is a priority system for *strategies*.

---

## Waterfall Sequence

```
  Strand 1: PLUCK ──── primary work from assigned workspace
       │ no work
       ▼
  Strand 2: EXPLORE ── look for work in other configured workspaces
       │ no work
       ▼
  Strand 3: MEND ───── cleanup: stale claims, orphaned locks, health
       │ nothing to clean
       ▼
  Strand 4: WEAVE ──── create beads from documentation gaps (opt-in)
       │ no gaps or disabled
       ▼
  Strand 5: UNRAVEL ── propose alternatives for HUMAN-blocked beads (opt-in)
       │ none or disabled
       ▼
  Strand 6: PULSE ──── codebase health scan, auto-generate beads (opt-in)
       │ no issues or disabled
       ▼
  Strand 7: KNOT ───── alert human, enter backoff
       │
       ▼
  → EXHAUSTED (backoff and retry from Strand 1)
```

---

## Strand 1: Pluck

**Purpose:** Process beads from the worker's assigned workspace. This is the primary work strand and will handle >90% of all bead processing.

**Invokes agent:** Yes.

**Entry condition:** Worker has an assigned workspace with a `.beads/` directory.

**Algorithm:**
1. Query bead store: `br ready --unassigned --json` in workspace
2. Filter: exclude beads with labels `deferred`, `human`, `blocked`
3. Filter: exclude beads in the current retry exclusion set
4. Sort: priority (ascending, 0 = highest), then creation time (ascending, oldest first)
5. Return sorted candidates for claiming

**Exit conditions:**
| Result | Action |
|--------|--------|
| Candidates found | Return `BeadFound(candidates)` → worker proceeds to CLAIMING |
| No candidates (queue empty) | Return `NoWork` → fall through to Strand 2 |
| Bead store error | Emit telemetry, return `Error` → fall through to Strand 2 |

**Determinism guarantee:** The sort key `(priority, created_at)` produces the same ordering for all workers viewing the same queue state. Workers will compete for the same top-priority bead, and the claim mechanism resolves contention.

---

## Strand 2: Explore

**Purpose:** Discover work in other configured workspaces when the home workspace is empty.

**Invokes agent:** No. Explore only finds candidates — execution happens back through the standard CLAIMING → DISPATCHING flow.

**Entry condition:** Strand 1 returned no work. Explore is enabled in config. At least one additional workspace is configured.

**Algorithm:**
1. Read configured workspace list from config (explicit paths, no filesystem scanning)
2. For each workspace (in configured order):
   a. Check `.beads/` directory exists
   b. Query `br ready --unassigned --json`
   c. If candidates found, return them with workspace context
3. If no workspace has work, return `NoWork`

**Exit conditions:**
| Result | Action |
|--------|--------|
| Candidates found in another workspace | Return `BeadFound(candidates)` with workspace override |
| No candidates in any workspace | Return `NoWork` → fall through to Strand 3 |

**Design notes (from `docs/notes/explore-strand-bugs.md`):**
- **No filesystem scanning.** NEEDLE-deprecated's `find`-based discovery caused 35+ CPU load with 40 workers. Workspaces must be explicitly configured.
- **No upward traversal.** The v1 explore strand walked up parent directories to `/home`, then `/`. This is eliminated.
- **Workspace list is static** for the duration of a session. It is read from config at boot and not re-evaluated.
- **Workers do not permanently relocate.** If a worker finds work in another workspace, it processes that bead and returns to its home workspace for the next cycle.

---

## Strand 3: Mend

**Purpose:** Maintenance and cleanup operations that keep the bead store healthy.

**Invokes agent:** No.

**Entry condition:** Strands 1-2 returned no work.

**Algorithm:**
1. **Stale claim cleanup:** Find beads with status `in_progress` where the assigned worker has no active heartbeat (TTL expired). Release them.
2. **Orphaned lock cleanup:** Find workspace lock files older than TTL. Remove them.
3. **Dependency cleanup:** Find closed beads that are still listed as blockers on open beads. Remove the stale dependency links.
4. **Database health:** Run `br doctor` (not `--repair` unless errors found).

**Exit conditions:**
| Result | Action |
|--------|--------|
| Cleanup performed | Return `WorkCreated` → restart from Strand 1 (released beads may now be claimable) |
| Nothing to clean | Return `NoWork` → fall through to Strand 4 |

**Design notes (from `docs/notes/bead-lifecycle-bugs.md`):**
- Stale dependency links caused permanent blocking in NEEDLE-deprecated. Mend must clean these.
- Distinguish "did work" from "found nothing" — v1 had an infinite loop where mend returned success on failed releases.

---

## Strand 4: Weave (opt-in)

**Purpose:** Analyze workspace documentation for gaps and create new beads to address them.

**Invokes agent:** Yes — uses the agent to analyze documentation and propose beads.

**Entry condition:** Strands 1-3 returned no work. Weave is explicitly enabled in workspace config (`strands.weave.enabled: true`).

**Algorithm:**
1. Identify documentation files (README, AGENTS.md, docs/, etc.)
2. Dispatch agent with gap-analysis prompt
3. Agent proposes new beads (as structured output)
4. Create beads via bead store
5. Return `WorkCreated` → restart from Strand 1

**Guardrails (from `docs/notes/self-modification-risks.md`):**
- **Max beads per weave run:** Configurable, default 5. Prevents unbounded bead creation.
- **Cooldown period:** Minimum time between weave runs per workspace, default 24h.
- **Seen-issues deduplication:** Track previously created weave beads to prevent duplicates.
- **Workspace exclusion:** Weave is disabled for NEEDLE's own workspace by default. Workers must not create work for their own orchestrator without human approval.
- **Human review label:** Weave-created beads are labeled `weave-generated` for easy filtering.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Beads created | Return `WorkCreated` → restart from Strand 1 |
| No gaps found | Return `NoWork` → fall through to Strand 5 |
| Disabled | Return `NoWork` → fall through to Strand 5 |

---

## Strand 5: Unravel (opt-in)

**Purpose:** For beads labeled `human` (requiring human decision), propose alternative approaches that an agent could execute instead.

**Invokes agent:** Yes — uses the agent to analyze the blocked bead and propose workarounds.

**Entry condition:** Strands 1-4 returned no work. Unravel is explicitly enabled. There are beads with `human` label in the workspace.

**Algorithm:**
1. Query beads with `human` label
2. For each (up to `max_unravel_per_run`, default 3):
   a. Dispatch agent with the bead context and a prompt asking for alternative approaches
   b. If agent proposes viable alternatives, create child beads with `alternative` label
   c. Do NOT close or modify the original `human` bead
3. Return `WorkCreated` if alternatives were created

**Guardrails:**
- Original `human` bead is never modified or closed
- Alternative beads are linked as children (informational, not blocking)
- Max alternatives per `human` bead: configurable, default 2
- Cooldown: don't re-analyze a `human` bead that was analyzed within the last 7 days

**Exit conditions:**
| Result | Action |
|--------|--------|
| Alternatives created | Return `WorkCreated` → restart from Strand 1 |
| No `human` beads or no alternatives viable | Return `NoWork` → fall through to Strand 6 |
| Disabled | Return `NoWork` → fall through to Strand 6 |

---

## Strand 6: Pulse (opt-in)

**Purpose:** Scan the codebase for health issues (stale TODOs, missing tests, dependency drift, linting) and create beads for significant findings.

**Invokes agent:** Yes — uses the agent (or external tools) to scan the codebase.

**Entry condition:** Strands 1-5 returned no work. Pulse is explicitly enabled. Cooldown has expired.

**Algorithm:**
1. Run configured scanners (linters, test coverage, dependency checkers, TODO scanners)
2. Compare results against previous scan (stored in `~/.needle/state/pulse/`)
3. For new issues exceeding severity threshold, create beads
4. Update last-scan state

**Guardrails:**
- **Max beads per pulse run:** Configurable, default 10
- **Cooldown:** Default 48h between scans
- **Severity threshold:** Only create beads for issues above configured severity
- **Deduplication:** Track seen issues to prevent duplicate beads across scans
- **Workspace exclusion:** Same as Weave — disabled for NEEDLE's own workspace by default

**Exit conditions:**
| Result | Action |
|--------|--------|
| Beads created | Return `WorkCreated` → restart from Strand 1 |
| No new issues | Return `NoWork` → fall through to Strand 7 |
| Disabled | Return `NoWork` → fall through to Strand 7 |

---

## Strand 7: Knot

**Purpose:** All work-finding strategies are exhausted. Alert the human and enter backoff.

**Invokes agent:** No.

**Entry condition:** Strands 1-6 all returned `NoWork`.

**Algorithm:**
1. Determine alert state:
   - **First time exhausted:** Emit `worker.idle` telemetry. Start backoff timer.
   - **Repeated exhaustion (>N cycles):** Create alert bead (if not already created within cooldown).
2. Verify before alerting (three-state check):
   a. **No beads exist:** Queue is genuinely empty. Normal idle.
   b. **All beads claimed:** Other workers are busy. Normal contention. Wait.
   c. **Beads invisible:** Configuration error (wrong workspace, broken filter). Alert.
3. Return `NoWork` → worker enters EXHAUSTED state with backoff

**Guardrails (from `docs/notes/worker-starvation-lessons.md`):**
- **Verify independently before alerting.** The v1 system had 100% false positive rate because it used the same broken code path for verification.
- **Three-state model.** "No work" is three different conditions with different responses. Conflating them caused the false positive spiral.
- **Rate limit alerts:** Max 1 alert bead per workspace per hour.
- **Alert includes diagnostics:** Bead counts, worker count, claimed count, config snapshot.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Always | Return `NoWork` → EXHAUSTED state |

---

## Strand Configuration

```yaml
# ~/.needle/config.yaml or .needle.yaml
strands:
  pluck:
    enabled: true           # always on, cannot be disabled
  explore:
    enabled: true
    workspaces:             # explicit list, no auto-discovery
      - /home/coder/project-a
      - /home/coder/project-b
  mend:
    enabled: true
    stale_claim_ttl: 300    # seconds before a claimed bead is considered stale
    lock_ttl: 600           # seconds before an orphaned lock is removed
  weave:
    enabled: false          # opt-in
    max_beads_per_run: 5
    cooldown_hours: 24
    exclude_workspaces: []  # workspaces where weave is forbidden
  unravel:
    enabled: false          # opt-in
    max_per_run: 3
    cooldown_days: 7
  pulse:
    enabled: false          # opt-in
    max_beads_per_run: 10
    cooldown_hours: 48
    severity_threshold: warning
    scanners:
      - name: todo-scanner
        command: "grep -rn 'TODO\\|FIXME' {workspace}/src"
      - name: test-coverage
        command: "cargo tarpaulin --skip-clean -o json"
  knot:
    enabled: true           # always on, cannot be disabled
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3 # cycles before creating alert bead
```
