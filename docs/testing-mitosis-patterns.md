# Mitosis Test Patterns and Workarounds

**Date:** 2026-09-14
**Bead:** needle-6729ce69 (documents the final state of the mitosis test-fix family)
**Verification lineage:** needle-6522fcf4 → needle-12609517 → needle-ad7bd937 →
needle-2cf1bff9 / needle-899bac1d / needle-a3112b51 → needle-771aeb12 /
needle-21383ec6 / needle-62f9ef8e → needle-ec1d8479 / needle-12609517 →
needle-23d823ab

---

## Overview

Mitosis is NEEDLE's bead-splitting mechanism: on a failed attempt, the worker
can ask an agent to decompose the bead into focused children. Its tests have
their own failure history — resource leaks, races, assertion flakiness, and a
fresh regression from an unrelated dispatch hardening — and this document
records the patterns and workarounds that make them pass deterministically.

Read this before writing or modifying a mitosis test. The interactions below
have each produced a real multi-day debugging cycle.

---

## What counts as the mitosis test suite

| Tier | Where | What it covers | State 2026-09-14 |
|---|---|---|---|
| Unit | `src/mitosis/mod.rs` `#[cfg(test)]` (+ `src/resolve::executor` split test) | Evaluation gates, dedup, caps (`max_children`/`max_depth`), depth labels, quarantine interplay, child creation | 111/111 pass (`cargo test --lib mitosis`) |
| P2 integration | `tests/p2_integration_tests.rs`, tests 12–14 + skip paths | `MitosisEvaluator` against mock stores through the real `Dispatcher` | 6/6 pass with the fixture rework; 3/6 at HEAD (see [Current state](#current-state)) |
| Span-depth regression | `tests/p2_integration_tests/claim_cycle_span_depth_regression.rs` | Claim cycles keep bead span depth constant (shares the dispatcher fixture) | Same split as above |
| Timeout-path policy | `tests/integration_tests/timeout_config_integration*.rs` | `mitosis.timeout_triggered` policy parsing and defaults (disabled by default) | Pass |
| E2E shell | `tests/e2e/{auto_split,forced_mitosis,failure_counter,failure_counter_persistence}.sh` | Full worker loop through a real workspace | **Blocked — not runnable** (see [Limitations](#remaining-limitations)) |

---

## Current state

Two facts must be read together; they were both verified on 2026-09-14.

1. **At commit `6cb82d41` (then-HEAD), the three dispatching P2 mitosis tests
   fail deterministically** — `mitosis_splits_multitask_bead_creates_children`,
   `mitosis_duplicate_split_creates_zero_new_children`,
   `mitosis_concurrent_workers_flock_serializes`, plus
   `claim_cycle_span_depth_regression` (same root cause). Verified across 3
   consecutive runs by needle-23d823ab; iad-ci agreed (all needle-ci runs that
   day failed).

2. **With the fixture rework applied, all of them pass** — 6/6 P2 mitosis +
   2/2 span-depth + 111/111 unit, in under a second combined. The rework
   (claim the parent in the fixture, wire the store into the dispatcher) was
   in flight in the shared worktree when this was written and is exactly the
   pattern described in the next section.

Root cause of the HEAD failures: commit `30d8f9c4` ("make dispatch fail closed
on every claim-verification error", needle-40ca2f6e) turned the pre-spawn
claim verifier from an optional check into a hard precondition. The mitosis
fixtures predated it and wired no bead store, so every dispatch aborted with:

```
no bead store wired for pre-spawn claim verification — refusing to spawn bead … unverified
```

The lesson generalizes: **a Dispatcher behavior change is a mitosis-test
breaking change even when mitosis code is untouched.** The fixtures exercise
the real dispatch path, on purpose.

---

## Known interaction patterns

### 1. Dispatching fixtures must claim the parent and wire the store

Because dispatch is fail-closed on claim verification (`src/dispatch/mod.rs`,
pre-spawn block), any fixture that drives `MitosisEvaluator` through a real
`Dispatcher` needs, in this order:

```rust
let store = Arc::new(ConcurrentMockStore::new(vec![parent.clone()]));
let claim = store
    .claim(&parent.id, MITOSIS_WORKER_ID)
    .await
    .expect("claim parent for mitosis fixture");
assert!(matches!(claim, ClaimResult::Claimed(_)));

let dispatcher = create_mitosis_dispatcher(json, store.clone()); // wires .with_bead_store(store)
```

`create_mitosis_dispatcher(json_response, store)` (bottom of
`tests/p2_integration_tests.rs`) builds a `mitosis-bash` adapter that echoes a
fixed JSON proposal, then `.with_bead_store(store)` and
`.with_worker_id(...)`. Never construct a dispatcher without a store for a
test that reaches `spawn` — evaluation-only tests (skip paths) are fine
without one.

### 2. Dedup tests need a store that shows its children

`mitosis_duplicate_split_creates_zero_new_children` relies on the second
`evaluate()` seeing the first split's children via `store.show()`. The
`MitosisDedupeStore` fixture exists precisely to reflect created children in
`show()`. A store whose `show()` omits children yields a second full split —
the test then "fails" even though mitosis is working. Dedup is implemented at
evaluation time against visible bead state, not enforced by the store.

### 3. Tempdir ownership must outlive the async tasks that use it

The flock-serialization test launches concurrent `evaluate()` calls that lock
inside the tempdir. If the `TempDir` is created inside the task, it is dropped
when the task's first await yields — and the lock directory vanishes under a
peer. The working shape:

```rust
let mut tempdirs = Vec::new();          // owns every TempDir for the whole test
for _ in 0..WORKERS {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let lock_dir = tempdir.path().to_path_buf();
    tempdirs.push(tempdir);             // keep alive until all tasks complete
    // ... spawn task using lock_dir ...
}
drop(lock_dir);
drop(ws);                               // explicit drop after joins, for clarity
```

Before commit `b367f6f3`, these tests created
`std::env::temp_dir().join("needle-test-*")` directories by hand and leaked
them on every failure path. Never go back to manual tempdir creation;
`tempfile::tempdir()` cleanup is scope-based and failure-safe.

### 4. The failure-count gate is a precedence chain, not three switches

`MitosisConfig` (`src/config/mod.rs`) gates evaluation through mutually
exclusive branches evaluated in this order (`src/mitosis/mod.rs`, evaluation
entry):

1. `force_failure_threshold > 0` — split only when `failure_count >= N`;
   while below the threshold mitosis is **skipped, not deferred**.
2. else `repeat_interval > 0` — fires at failure 1, 1+N, 1+2N, …; beads
   already carrying a `mitosis-depth` label are skipped.
3. else `first_failure_only` — fire only at failure 1.

Tests routinely get this wrong by setting `force_failure_threshold: 3` and
expecting a split on failure 1. The recorded E2E variant of the same mistake
(needle-77265c60): `forced_mitosis.sh` set `force_failure_threshold: 3` while
the outer quarantine threshold defaulted to 5, so the parent never split, its
children kept failing, and the run only ended when the harness `timeout`
killed the worker (exit 124). Mitosis was functioning correctly the whole
time. Pick one knob, set the others to 0, and assert against the branch you
actually selected.

### 5. Skip-path labels silently suppress evaluation

Beads carrying `mitosis-child`, a `mitosis-depth:N` label, or any
quarantine-family label (`quarantine*`, `cycling`, `verification-failed`) are
skipped without an error — by design, that is the explosion guard (see
`docs/notes/mitosis-explosion-postmortem.md`: 5,741 beads from one
unsplittable parent when this guard silently failed). Fixtures that reuse a
bead or clone label sets can inherit a skip label and observe a silent
`Skipped` where a split was expected. When a split does not happen, first
dump the fixture bead's labels.

### 6. Adapter-not-found is a Skip, not an error

`mitosis_evaluator_adapter_not_found_skips` pins the contract that an unknown
adapter produces `MitosisResult::Skipped` rather than a dispatch error. New
error-handling in `resolve_adapter` or `Dispatcher` must preserve this: the
evaluator treats "cannot ask the agent" as "do not split", because a
misconfigured worker must not convert a config mistake into bead-tree churn.

### 7. E2E assertions need explicit flush + retry, and a tolerant exit code

The shell E2E tier talks to a real worker and a real store, so state lands
asynchronously. Commit `66790ee5` established the working sequence, and any
new E2E mitosis check needs the same four pieces:

1. Wait (poll up to ~5 s) for the telemetry `.jsonl` to appear and be
   non-empty before asserting on events.
2. Force a store flush (`sync --flush-only`) after the worker exits and
   before reading bead state.
3. Retry store reads up to 3 times with short sleeps — indexing delays
   produced one-shot false negatives.
4. Accept **both exit codes 0 and 1** from the worker: 0 means the queue
   was exhausted, 1 means it stopped after processing available work. Both
   are clean terminations; asserting `== 0` flaked on queue ordering.

### 8. Local runs compile other workers' in-flight work

This repo is a single shared checkout; a plain `cargo test` builds whatever is
in the worktree right now, including another worker's uncommitted WIP in
`src/`. That is how a mitosis test can start "failing" (or passing) with no
mitosis commit in sight. When attributing a local result to a commit:

- Check `git status` / `git diff HEAD --stat` first; identify WIP that is not
  yours.
- To verify the committed tree, use the worktree-restore sim: snapshot dirty
  files, restore to pure HEAD (`git diff --name-only HEAD`, never touch the
  index), test, restore byte-identical.
- CI (needle-ci on iad-ci) builds the commit, not the worktree — it is the
  only result that counts for "is HEAD red".

The 30d8f9c4 regression documented above was found this way: the fixtures'
fix landed as another worker's WIP while the committed tree stayed red.

---

## Workarounds implemented (commit index)

| Commit | Bead | Workaround | Pattern |
|---|---|---|---|
| `b367f6f3` | needle-2cf1bff9 | `tempfile::tempdir()` per worker; `Vec` keeps TempDirs alive past task joins; explicit `drop()` | [3](#3-tempdir-ownership-must-outlive-the-async-tasks-that-use-it) |
| `871f3c69` | needle-a3112b51 | E2E `forced_mitosis.sh` timeout 60 s → 180 s (3 failure cycles ≈ 90 s + evaluation ≈ 20 s + child claim ≈ 10 s + margin) | [4](#4-the-failure-count-gate-is-a-precedence-chain-not-three-switches), [Limitations](#remaining-limitations) |
| `66790ee5` | needle-899bac1d | Telemetry flush wait, `sync --flush-only`, 3× retry on store reads, exit code 0-or-1 accepted | [7](#7-e2e-assertions-need-explicit-flush--retry-and-a-tolerant-exit-code) |
| `ebc9b72a` | needle-29f5d663 | Cached `qualified_id` on `Worker`; dispatcher uses it instead of raw `worker_name` — worker-name inconsistency was failing claim verification | [1](#1-dispatching-fixtures-must-claim-the-parent-and-wire-the-store) |
| (in flight at doc time) | needle-40ca2f6e follow-up | Fixture rework: claim parent + `create_mitosis_dispatcher(json, store)` | [1](#1-dispatching-fixtures-must-claim-the-parent-and-wire-the-store), [Current state](#current-state) |
| `6656e902` / `6f84ebc3` | needle-37aedffe | `max_children` (8) / `max_depth` (2) caps — bounds the blast radius a mitosis test can create | [5](#5-skip-path-labels-silently-suppress-evaluation) |

---

## Guidance for future mitosis test development

**Prefer the in-process evaluator over E2E.** `MitosisEvaluator` + mock stores
run the whole family in ~0.15 s and need no isolation beyond a tempdir. Reach
for the E2E shell tier only for behavior that genuinely requires the worker
loop (real claim, real failure counting, real telemetry).

**Model the fixture on `create_mitosis_dispatcher`.** One helper, fixed JSON
proposal, store wired, worker id set. If your test needs a different proposal,
parameterize the JSON — do not build a second dispatcher variant.

**Every new test gets isolation at creation.** Follow
`docs/testing-isolation-patterns.md`: tempdir-backed state only, no real
`$HOME`, no writes outside the test's own prefix. The 2026-07-20 contamination
incident (ADR-006) started with a test that scanned the real home.

**Assert on the gate you configured.** Before writing the assertion, write
down which branch of the precedence chain (pattern 4) your config selects.
If `force_failure_threshold > 0`, failure 1 does not split.

**Keep the skip-label contract covered.** Any new skip condition (label,
depth, config detection) needs a unit test asserting `Skipped`, and a
negative test that the guard does not over-block legitimate re-splits —
Phase 2 of the explosion postmortem is what over-blocking looks like.

**Adding a new test *target* file** (not just a test in
`tests/p2_integration_tests.rs`) silently breaks the ci-deps builder image.
Update the stubs in `ci/Dockerfile.ci-deps` and bump `ci/VERSION` in the same
change.

**E2E scripts must target the `bead` CLI.** The existing mitosis E2E scripts
call `br`, which is prohibited on this box after the bead-rs migration. New
E2E work goes through `bead` (or, better, moves the assertion down into the
in-process tiers).

**Run order for local iteration:**

```bash
cargo test --lib mitosis                                   # unit family (111)
cargo test --test p2_integration_tests -- mitosis          # P2 family (6)
cargo test --test p2_integration_tests -- span_depth       # shared-fixture regression
```

Use `--test-threads=2` when reproducing interaction reports; the flock test
 deadlocks under some thread counts when its tempdir is mis-owned (pattern 3),
and a `#[tokio::test]`-native timeout on that test is the tripwire, not a fix.

---

## Remaining limitations

1. **The E2E mitosis tier is not runnable.** `auto_split.sh`,
   `forced_mitosis.sh`, `failure_counter.sh`,
   `failure_counter_persistence.sh` (and the rest of that directory) invoke
   the deprecated `br` CLI, which must not be invoked on this box post
   bead-rs migration. They need a bead-rs port; until then the E2E tier is
   unverified by definition, and the P2 tier is the only dispatch-path
   coverage. (Tracked as the remaining suite gap in needle-23d823ab.)
2. **`forced_mitosis.sh` has a latent design bug independent of the CLI
   port**: `force_failure_threshold: 3` vs the default quarantine threshold
   of 5 (pattern 4). A port that preserves the config verbatim will still
   time out at exit 124.
3. **At HEAD `6cb82d41` the committed tree is red** for the four
   store-wired tests until the fixture rework lands; do not attribute new
   failures on top of it without checking [Current state](#current-state)
   first.
4. **A handful of lib-suite tests fail deterministically in this box's
   environment** (~31, unrelated to mitosis — cargo-shim and env-dependent).
   Diff your failure list against that known set before attributing a local
   failure; gate on CI.
5. **Timeout-triggered mitosis has no end-to-end test.** The policy config
   has unit coverage (`timeout_config_integration*`), but the route from a
   hard timeout through decomposition to reordered children is open work
   (needle-2fece230, needle-c84b6ded).
6. **Blast radius is bounded, not small.** Caps (`max_children: 8`,
   `max_depth: 2`) still admit ~73 beads from one unsplittable parent
   (needle-3f82395d); a runaway fixture can still create real children if
   pointed at a real store. Mock stores only.

---

## References

- `docs/research-mitosis-implementation-and-failure-semantics.md` — the two
  evaluation paths (ordinary failure vs timeout) and their semantics
- `docs/timeout-mitosis-decomposition-design.md` — timeout-path design
- `docs/adr/011-mitosis-timeout-child-reaping.md` — process-group reaping on
  outer-timeout cancellation
- `docs/notes/mitosis-explosion-postmortem.md` — why the skip/depth guards
  exist
- `docs/testing-isolation-patterns.md` — the isolation rules every test
  inherits
- `tests/README.md` — known environment-dependent tests
- Verification records: needle-23d823ab (HEAD red + root cause),
  needle-77265c60 (6/6 + E2E design issue), needle-771aeb12 / needle-21383ec6
  / needle-62f9ef8e (per-fix verification)
