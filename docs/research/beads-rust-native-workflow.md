# beads_rust (br) Native Workflow and Processing Model

## Research Date: 2026-03-20
## Source: https://github.com/Dicklesworthstone/beads_rust

## What It Is

beads_rust (`br`) is the Rust port of Steve Yegge's beads issue tracker. It is the tool NEEDLE wraps. Written by Dicklesworthstone, it provides ~20,000 LOC of core issue tracking with SQLite + JSONL hybrid storage. It deliberately freezes at this architecture while the original `bd` evolves toward Dolt and GasTown.

## Native Bead Lifecycle

```
create -> [open] -> update --status in_progress --assignee "agent" -> [in_progress] -> close --reason "done" -> [closed]
                                                                                    -> reopen -> [open]
```

### Key Commands

| Command | Purpose |
|---------|---------|
| `br create "Title" --type bug --priority 1` | Create a new bead |
| `br ready` | List open, unblocked beads (no open dependencies) |
| `br ready --json` | Machine-readable ready queue |
| `br update bd-abc --status in_progress --assignee "x"` | Claim a bead |
| `br close bd-abc --reason "Fixed"` | Close a bead |
| `br dep add bd-child bd-parent` | Create dependency (child blocks parent) |
| `br list --json` | List all beads with full metadata |
| `br show bd-abc --json` | Show single bead details |
| `br search "query"` | Full-text search |
| `br sync --flush-only` | Export SQLite to JSONL |
| `br sync --import-only` | Import JSONL to SQLite |

### Priority Model

Beads have integer priorities (0 = highest). The `br ready` command returns unblocked beads but does NOT sort by priority -- that ordering is left to the consumer. NEEDLE must impose its own deterministic priority sort.

### Dependency Graph

Dependencies create blocking relationships:
- `br dep add bd-child bd-parent` means bd-parent is blocked until bd-child closes
- `br ready` only returns beads with no open blockers
- Cycle detection prevents circular dependencies

## Storage Architecture

### SQLite (.beads/beads.db)

- Primary storage for fast local queries
- WAL mode for concurrent readers
- Single-writer semantics (no built-in multi-writer coordination)
- Uses FrankenSQLite (embedded Rust SQLite) which has known corruption issues with partial indexes (issue #171)

### JSONL (.beads/issues.jsonl)

- Append-friendly format for git collaboration
- Each line is a complete issue record
- Authoritative data source (survives SQLite corruption)
- Sync between SQLite and JSONL is manual: `br sync --flush-only` / `--import-only`
- Auto-import/auto-flush can trigger on every command but causes conflicts under concurrent access (issue #191)

## Concurrency Model (and Its Limitations)

### What br Provides

- SQLite transaction isolation for individual operations
- Atomic file writes via temp-file + rename for JSONL
- Content hashing for change detection
- Staleness detection comparing SQLite and JSONL timestamps

### What br Does NOT Provide

- No file-level locking between processes
- No atomic claim operation (must set status + assignee in separate conceptual steps within one `br update`)
- No heartbeat or lock expiry
- No coordination server or daemon
- No distributed locking primitives

### Known Concurrency Bugs

**Issue #171 (FrankenSQLite Corruption)**:
FrankenSQLite's partial index maintenance produces on-disk formats that standard sqlite3 flags as corrupted. `br doctor` uses the same engine and cannot detect this. Recovery: delete beads.db, rebuild from JSONL via `br sync --import`.

**Issue #191 (Concurrent SyncConflict)**:
When 3+ processes run simultaneously, auto-import detects JSONL changes from other processes and falsely flags a SyncConflict. Workaround: `--no-auto-import --no-auto-flush` with manual sync management.

## Agent-First Design

br is designed for machine consumption:
- `--json` flag on every command for structured output
- `--quiet` for minimal feedback
- `--no-color` for piped environments
- Auto-detects TTY vs. pipe for output formatting
- `RUST_LOG=error` suppresses internal logs

## How NEEDLE Uses br

NEEDLE's interaction with br follows this pattern per cycle:

1. **SELECT**: `br ready --json` to get unblocked beads, then sort by priority + creation time
2. **CLAIM**: `br update bd-xxx --status in_progress --assignee "needle-alpha"` (atomic within SQLite transaction, but not protected against concurrent br processes)
3. **BUILD**: `br show bd-xxx --json` to get full bead context for prompt construction
4. **OUTCOME/SUCCESS**: `br close bd-xxx --reason "Completed by needle-alpha"`
5. **OUTCOME/FAILURE**: `br update bd-xxx --status open --assignee ""` to release

## Design Decisions Relevant to NEEDLE

1. **No built-in orchestration**: br is a storage layer, not a workflow engine. All orchestration logic lives in NEEDLE.

2. **No claim primitive**: Unlike `bd --claim` which atomically sets assignee + in_progress, br requires explicit `--status` and `--assignee` flags. The atomicity is at the SQLite transaction level, not at the bead-lifecycle level.

3. **JSONL is the safety net**: When SQLite corrupts (and it does), JSONL is the recovery path. NEEDLE must tolerate database corruption and know how to trigger recovery (`br doctor --repair` or full rebuild).

4. **Priority ordering is external**: br does not impose ordering on `br ready` results. NEEDLE must implement its own deterministic sort.

5. **No worker awareness**: br has no concept of workers, sessions, or fleet management. All coordination is NEEDLE's responsibility.
