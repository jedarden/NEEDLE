# ADR-023: Gate Execution Errors Are Infrastructure, Not Bead Failures

**Status:** Accepted — 2026-08-29
**Deciders:** operator (jedarden), via Claude Code
**Extends:** ADR-020 (verification gates judge committed state). Does not change what a gate judges; changes what happens when the gate cannot judge.
**Tracking:** Phase 19 §19.1; beads under label `phase-19`

## Context

ADR-020 made verification gates the arbiter of acceptance. The outcome handler treats any non-passing gate as `Failure`, which increments the bead's `failure-count` and, at five, quarantines it (ADR-012).

In the seven days to 2026-08-29 the ex44 fleet logged **2,470** gate failures. **2,346** of them (95%) were the string `failed to execute command: No such file or directory`. The gate never ran:

- `outcome/mod.rs:194` constructed the gate from `bead.workspace`, which is unset for any bead claimed in the worker's home workspace, so `sh` was spawned with `current_dir("")` and failed with ENOENT (fixed in `8818f9e9`).
- spaxel and mta-my-way configured `verification: [/home/coding/.needle/hooks/verify-changes.sh]`, a path that has never existed on any host (removed 2026-08-29).

Every one of those non-runs counted against a bead. 138 beads reached quarantine — including the P0 bead whose job was to fix acceptance — on failures that said nothing about their work. Meanwhile the fleet kept dispatching into the same broken gate, burning a dispatch per bead per cycle, because nothing distinguished "your work failed the check" from "the check could not run".

## Decision

1. **A new outcome class, `GateError`,** covers every case in which the gate produced no verdict: the command could not be spawned, its working directory is missing or empty, the configured command does not exist in the workspace, or the gate timed out before reporting. A gate that ran to completion and failed remains `Failure`.
2. **`GateError` releases the bead and leaves `failure-count` untouched.** It emits `gate.execution_error` with workspace, gate name, command, and reason.
3. **Three consecutive `GateError`s degrade the workspace, not the beads.** A `gate-degraded` workspace is skipped for ordinary dispatch by Pluck and Explore. Exactly one fingerprinted, claimable P0 bead — `Gate broken: <command> — <reason>` — is surfaced there, because repairing a gate is itself verifiable by running the gate. The first successful gate run clears the degradation and closes the bead.
4. **Configuration is validated before dispatch.** A `gates:`/`verification:` entry naming a nonexistent path is a `needle doctor` FAIL and a worker-boot warning; the fleet does not learn about it one bead at a time.

## Alternatives considered

- **Skip the gate when it cannot run and accept the work.** Rejected: that is acceptance-by-self-report, the exact state ADR-020 exists to end. A broken gate must stop dispatch in that workspace, not open it.
- **Treat `GateError` as a Tier-1 transient error and retry the same bead.** Rejected: the cause is per-workspace, not per-bead; retrying the same bead against the same broken gate produces the same ENOENT and, worse, keeps the bead claimed.
- **Fix the two concrete causes and leave the classification alone.** Rejected: both were fixed the same day, and the class of failure ("the check could not run") will recur with the next path typo or missing tool. The classification is the durable fix; the two bugs were the symptom.

## Consequences

- A workspace with a broken gate goes quiet within three dispatches and announces itself with a single bead an agent can act on; its beads keep their failure history intact.
- `failure-count` regains its meaning as a count of *work* that failed verification.
- `needle doctor` catches the most common gate misconfiguration (a path that does not exist) at install time.
