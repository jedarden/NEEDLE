# Implementation Phases

NEEDLE is built in three phases. Each phase produces a usable tool. No phase depends on future phases — Phase 1 alone is a complete, working system.

---

## Phase 1: Core State Machine

**Goal:** A single binary that processes beads from one workspace using one agent. The state machine is complete. Telemetry is complete. The tool works end-to-end.

### Deliverables

| Component | Scope |
|-----------|-------|
| **CLI** | `needle run`, `needle stop`, `needle list`, `needle version` |
| **Worker** | Full state machine: BOOTING → SELECTING → CLAIMING → BUILDING → DISPATCHING → EXECUTING → HANDLING → LOGGING → (loop) |
| **Strand 1 (Pluck)** | Query, filter, sort beads from single workspace |
| **Strand 7 (Knot)** | Basic exhaustion handling (backoff, exit) |
| **Claimer** | Atomic claim via `br update --claim`, single workspace flock |
| **PromptBuilder** | Deterministic prompt from bead context |
| **Dispatcher** | Agent adapter loading, process execution, timeout enforcement |
| **OutcomeHandler** | All 6 outcomes handled (success, failure, timeout, crash, agent_not_found, interrupted) |
| **Telemetry** | File sink (JSONL), all events in catalog |
| **Config** | Global config file, CLI argument overrides |
| **Agent adapters** | Claude Code built-in, generic template |
| **BeadStore** | `br` CLI wrapper with JSON parsing |
| **Types** | All enums (State, Outcome, ClaimResult, StrandResult) with exhaustive matching |
| **tmux** | Session creation, naming, `needle run` self-invokes into tmux |

### Not in Phase 1

- Multi-worker coordination (flock is present but only one worker)
- Strands 2-6
- Heartbeat system
- Peer monitoring
- Workspace config (.needle.yaml)
- Multiple agent adapters
- Cost tracking
- Budget enforcement
- `needle attach`, `needle status`, `needle config`

### Success Criteria

- [ ] `needle run --workspace /path --agent claude-anthropic-sonnet` launches a worker in tmux
- [ ] Worker claims a bead, dispatches to Claude Code, handles outcome
- [ ] All 6 outcome paths tested with mock agent (exit 0, 1, 124, 127, 130, timeout)
- [ ] Telemetry JSONL file contains events for every state transition
- [ ] `needle list` shows running workers
- [ ] `needle stop` gracefully stops a worker (releases claimed bead)
- [ ] Worker loops: after handling one bead, it selects the next
- [ ] Worker exhausts: when no beads remain, enters backoff and eventually exits
- [ ] Binary compiles for Linux x86_64 and macOS arm64

### Estimated Scope

~15 source files, ~3,000 LOC (Rust). The state machine is the skeleton — most code is in the types, transitions, and error handling.

---

## Phase 2: Multi-Worker Fleet

**Goal:** Multiple workers operate in the same environment. They coordinate through shared state, detect failures, and self-heal. Workers roam across workspaces.

### Deliverables

| Component | Scope |
|-----------|-------|
| **Multi-worker launch** | `needle run --count N` with staggered startup |
| **Workspace flock** | Per-workspace claim serialization |
| **Heartbeat** | File-based heartbeat emission and monitoring |
| **Peer monitoring** | Stale/crashed worker detection |
| **Strand 2 (Explore)** | Roam configured workspaces for work |
| **Strand 3 (Mend)** | Stale claim cleanup, orphaned locks, dependency repair, db health |
| **Worker state registry** | Shared fleet state file |
| **Concurrency limits** | Provider/model max_concurrent, RPM limiting |
| **Workspace config** | `.needle.yaml` per-workspace overrides |
| **Additional adapters** | OpenCode, Codex, Aider built-in |
| **Cost tracking** | Token extraction, pricing config, effort events |
| **Budget enforcement** | Warn/stop at daily cost thresholds |
| **CLI extensions** | `needle attach`, `needle status`, `needle config` |
| **Database recovery** | Auto-detect corruption, repair from JSONL |

### Not in Phase 2

- Strands 4-6 (Weave, Unravel, Pulse)
- Hook sink for telemetry
- External webhook integrations
- `needle doctor` command
- `needle upgrade` self-update

### Success Criteria

- [ ] `needle run --count 5` launches 5 workers with staggered startup
- [ ] Workers claim different beads (no thundering herd — verify via telemetry)
- [ ] Crashed worker's claimed bead is released by peer within heartbeat_ttl
- [ ] Workers discover work in other configured workspaces (Explore strand)
- [ ] Mend strand cleans stale claims and orphaned locks
- [ ] Provider concurrency limits enforced (>N workers for same provider wait)
- [ ] `needle status` shows fleet summary with per-worker and per-bead stats
- [ ] `needle attach alpha` connects to a running worker's tmux session
- [ ] Cost tracking produces accurate estimates (±20% of actual)
- [ ] Database corruption is detected and auto-repaired
- [ ] Workspace `.needle.yaml` overrides global config correctly

### Estimated Scope

~10 additional source files, ~4,000 additional LOC. The concurrency layer is the most complex part.

---

## Phase 3: Advanced Strands and Operations

**Goal:** NEEDLE can create work (not just process it), monitor codebase health, and integrate with external systems. Full operational tooling.

### Deliverables

| Component | Scope |
|-----------|-------|
| **Strand 4 (Weave)** | Gap analysis, bead creation from documentation |
| **Strand 5 (Unravel)** | Alternative proposals for HUMAN-blocked beads |
| **Strand 6 (Pulse)** | Codebase health scans, auto-bead creation |
| **Validation gates** | Pluggable pre-closure validation (inspired by bg-gate) |
| **Hook sink** | Telemetry dispatch to webhooks/commands |
| **Self-update** | `needle upgrade` with version checking |
| **Doctor command** | `needle doctor` for full system health check |
| **Telemetry queries** | `needle logs --filter`, `needle status --cost` |
| **Installer** | One-liner install script, GitHub releases |

### Success Criteria

- [ ] Weave strand creates valid beads from documentation gaps (with guardrails)
- [ ] Unravel strand proposes alternatives for HUMAN beads without modifying originals
- [ ] Pulse strand detects codebase issues and creates beads (with deduplication)
- [ ] All opt-in strands respect cooldowns and max-bead limits
- [ ] Validation gates block bead closure when tests fail
- [ ] Hook sink delivers events to configured webhooks
- [ ] `needle upgrade` downloads and installs new version
- [ ] `needle doctor` reports system health across all subsystems
- [ ] One-liner install works on Linux and macOS

### Estimated Scope

~8 additional source files, ~3,000 additional LOC. Strands 4-6 are the bulk, with validation gates and hook system being smaller.

---

## Migration from v1

NEEDLE v2 is a clean rewrite. There is no in-place upgrade path from v1.

### Coexistence

During transition, v1 and v2 can coexist:

- v1 is installed at `~/.local/bin/needle` (bash script)
- v2 is installed at `~/.local/bin/needle` (compiled binary, replaces v1)
- Both use `~/.needle/` for config but v2's config schema differs
- Both read `.beads/` directories (same bead format)
- v1's tmux sessions use the same naming convention

### Migration Steps

1. Stop all v1 workers: `needle stop --all` (v1)
2. Back up v1 config: `cp -r ~/.needle ~/.needle-v1-backup`
3. Install v2 binary (overwrites v1)
4. Create v2 config: `needle init` (v2 detects and migrates compatible settings)
5. Test with single worker: `needle run --workspace /path --count 1`
6. Scale up: `needle run --count N`

### What Carries Over

- `.beads/` directories (unchanged, same `br` backend)
- Workspace structure
- Agent CLIs (same Claude Code, OpenCode, etc.)

### What Does Not Carry Over

- Config format (new YAML schema)
- Telemetry logs (new JSONL schema)
- Worker state files (new format)
- v1's build system, source files, and bash modules

---

## Test Strategy

### Unit Tests

Every module has unit tests. Key coverage targets:

| Module | Key Tests |
|--------|-----------|
| `outcome` | Every exit code maps to correct outcome variant |
| `strand` | Each strand returns correct StrandResult for each scenario |
| `claim` | Race lost, success, store error, max retries |
| `config` | Precedence: CLI > env > workspace > global > default |
| `telemetry` | Events serialized correctly, sequence monotonic |
| `health` | Stale detection, crashed vs stuck distinction |
| `bead_store` | JSON parsing handles all `br` output formats, including errors |
| `prompt` | Deterministic: same bead → same prompt hash |

### Integration Tests

| Test | What It Validates |
|------|-------------------|
| **End-to-end single worker** | Full loop: select → claim → build → dispatch (mock agent) → outcome → log |
| **Multi-worker claiming** | N workers, M beads: all beads claimed exactly once, no duplicates |
| **Crash recovery** | Kill worker mid-execution, verify peer releases claim |
| **Database corruption** | Corrupt SQLite, verify auto-repair and continued operation |
| **Timeout enforcement** | Agent that sleeps forever is killed after timeout |
| **Strand waterfall** | Empty workspace → explore → mend → knot progression |

### Property Tests

| Property | Description |
|----------|-------------|
| **Deterministic ordering** | For any queue state, all workers compute the same candidate ordering |
| **Exhaustive outcomes** | The outcome enum covers all possible exit codes (no `_` wildcard) |
| **Claim exclusivity** | Given N concurrent claim attempts on 1 bead, exactly 1 succeeds |
| **Heartbeat liveness** | A healthy worker's heartbeat is always within TTL |

### No Mocking of `br`

From `docs/notes/mitosis-explosion-postmortem.md`: v1's tests mocked `br` output and missed that `br show --json` never included labels. v2 integration tests run against a real `br` instance with a test `.beads/` directory.
