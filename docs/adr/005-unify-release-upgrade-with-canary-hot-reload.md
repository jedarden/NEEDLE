# ADR-005: Unify the GitHub-Release Upgrade Path with the Canary-Gated Hot-Reload Channel

**Status:** Accepted — 2026-08-12 (proposed 2026-07-20; see Addendum for acceptance re-verification)
**Deciders:** operator (jedarden), via Claude Code (fleet-wide deployed-artifact improvement review)
**Tracking:** plan.md Phase 9; implementation beads in this repo's workspace, labeled `artifact-improvement`

## Context

NEEDLE ships two binary-update mechanisms that share vocabulary (`:testing` / `:stable`, "canary", "hot-reload") but are structurally disconnected — verified by direct code read, not assumption:

1. **Manual GitHub-release upgrade** — `needle upgrade` (`perform_upgrade()`, `src/upgrade/mod.rs`). Calls `check_for_update()` against the GitHub Releases API, downloads the new binary, and `fs::rename`s it directly over `env::current_exe()` — in place, on whichever host and path the operator happens to be running it from. No canary validation of any kind. No fleet propagation: every host needs a human to run this individually, and nothing ever does so automatically.

2. **Self-modification canary/hot-reload channel** — `src/canary/mod.rs` plus `check_auto_canary()` / `check_hot_reload()` in `src/worker/mod.rs`. This is a real, already-implemented, already-tested automatic pipeline: every worker's own loop, on every cycle, between the LOGGING and SELECTING states —
   - `check_auto_canary()`: if `~/.needle/bin/needle-testing` exists, runs the canary suite against it in the `~/.needle/canary/` workspace; on an all-pass report, calls `runner.promote()`, which moves `needle-testing` → `needle-stable` (backing up the previous `needle-stable` → `needle-stable.prev` for rollback);
   - `check_hot_reload()`: independently, on every cycle, hashes the currently-running binary against `~/.needle/bin/needle-stable`; on a mismatch, calls `re_exec_stable()`, which `exec()`s into the new binary with `--resume --identifier <worker_name>`, preserving worker identity, at a safe loop boundary (never mid-dispatch).

   This machinery works and is unit-tested (`promote_moves_testing_to_stable`, `promote_first_time_no_existing_stable`, etc. in `src/canary/mod.rs`). But `check_auto_canary()` is gated behind `self.config.self_modification.enabled && self.config.self_modification.auto_promote` — both `false` in the live fleet config (`.needle.yaml`: `self_modification: { enabled: false, auto_promote: false, ... }`) — and, independent of that gate, **nothing in the codebase ever writes a `needle-testing` binary from a GitHub release.** The only producer of `needle-testing` today is the (currently disabled) self-modification pipeline, where an agent edits NEEDLE's own source, builds it locally, and drops the result at that path. `check_for_update()` / `perform_upgrade()` never touch `~/.needle/bin/` at all.

**Live evidence, gathered during this audit (2026-07-20, ex44 host):**

```
$ needle --version
needle 0.2.11
$ curl -s https://api.github.com/repos/jedarden/NEEDLE/releases/latest | jq -r '.tag_name, .published_at'
v0.2.12
2026-07-20T12:49:30Z
```

The installed binary on the very host running this audit is one release behind, published the same day. Neither existing update path would ever notice or correct this on its own: `needle upgrade` requires a human to run it; the canary/hot-reload channel is structurally blind to GitHub releases. `ps aux` and `needle list`/`needle status` at the time of this audit showed zero live NEEDLE processes on ex44 (fleet activity currently concentrated elsewhere), so there was no live worker mid-loop to observe skipping the check — but the same gap applies whether or not a worker happens to be running at any given moment, since neither path is wired to react to a new release regardless.

The fleet runs `worker.max_workers: 10` per host across at least two hosts (ex44, lab per CLAUDE.md), each worker a long-lived, independent tmux-session loop (README: "Independence — each worker is a self-contained loop in its own tmux session," "no central orchestrator"). Any fix has to preserve that no-central-orchestrator property rather than introduce a controller that pushes upgrades to hosts.

## Decision

1. **Route GitHub releases through the existing `:testing` slot, not a new path.** Add a download step that fetches a new GitHub release (reusing `check_for_update()`'s version-comparison logic) and writes it to `~/.needle/bin/needle-testing` — the same drop point the self-modification pipeline already uses — instead of `perform_upgrade()`'s current direct in-place overwrite. This means zero new promotion or propagation code: `check_auto_canary()` and `check_hot_reload()`, already running in every worker's loop today, pick up a release-sourced `:testing` binary exactly as they would a self-modification-sourced one.

2. **Own the periodic check in `needle supervise`, not in the worker loop.** The supervisor daemon already runs continuously per host, independent of bead dispatch, and already makes fleet-wide operational decisions (auto-scaling by queue depth). Add a configurable poll (`supervisor.update_check_interval_secs`, default 6h — infrequent enough to stay well clear of GitHub API rate limits across a multi-host fleet, frequent enough that drift like the 0.2.11/0.2.12 case above self-corrects same-day) that calls `check_for_update()` and, if newer, downloads to `needle-testing`.

3. **New gate, `supervisor.auto_upgrade_check: bool` (default `false`)** — independent of `self_modification.enabled`. A tagged, published GitHub release is a materially different trust level than an agent's own uncommitted self-edit (someone deliberately cut and published it); requiring the full self-modification story to be enabled just to get canary-gated propagation of *official* releases would conflate two different risk profiles. Reuse `self_modification.auto_promote` for whether promotion after a passing canary is automatic vs. requiring a manual `needle canary` step — it already means exactly that decision; do not add a second flag for the same semantic.

4. **Leave `needle upgrade` / `perform_upgrade()` as-is** for the manual, immediate, single-host case (e.g., bootstrapping a fresh install where no `:stable` exists yet) — this decision adds a second, automatic, canary-gated path; it does not remove the existing manual one.

5. **Out of scope for this decision:** auto-rollback triggered by post-upgrade outcome-rate anomalies. `needle rollback` (→ `:stable.prev`) already exists as a manual escape hatch; wiring it to fire automatically is a reasonable future hardening step once `auto_upgrade_check` has run in production, not a prerequisite for it.

## Alternatives Considered

1. **Canary-validate inside `perform_upgrade()` itself, keep it manual-only.** Rejected as the primary fix: still requires a human to remember to run `needle upgrade` on every host, which is exactly the condition that produced the observed drift (nobody ran it on ex44 despite the release being hours old). Worth doing as a minor independent hardening (today, even the manual path installs with zero validation) — filed as a separate, smaller bead rather than folded into this decision.
2. **Central push** — a control host SSHes into every fleet host and runs `needle upgrade`. Rejected: reintroduces the single controller NEEDLE's own design explicitly avoids (README: "no central orchestrator... coordination happens through the shared bead queue"), and no host-inventory/SSH-fanout tooling exists for the fleet today — this would be new infrastructure bolted on sideways rather than reuse of what's already built.
3. **Check on every worker-loop iteration instead of via the supervisor.** Rejected: couples a GitHub API call and a binary download to bead-dispatch latency, and roaming/short-lived workers (Explore strand) don't have a reliable idle moment to safely absorb that cost in. The supervisor already exists specifically as the per-host, dispatch-independent decision-maker; this is what it's for.
4. **Do nothing automatic — just have `needle status` print "N releases behind."** Rejected as a complete fix (a human still has to notice and act, per host, per release — the same failure mode observed), but cheap enough to ship immediately as a stopgap while the real mechanism lands; filed separately.

## Consequences

- **Positive:** Closes the exact drift class observed live during this audit without inventing a new, weaker "just overwrite the binary" auto-update mechanism — it reuses the canary gate, promotion, and hot-reload-at-safe-boundary machinery that already exists, is already unit-tested, and already has a rollback story (`needle rollback`).
- **Positive:** Preserves the no-central-orchestrator design principle — every host polls GitHub and canary-validates independently against its own local canary workspace; no host's upgrade depends on any other host or on a controller.
- **Risk:** The canary suite's existing fixtures were designed and tuned against agent-authored source-level self-modifications. It is not yet confirmed those same fixtures give adequate coverage for a full official-release binary swap, which can legitimately change more surface at once (new CLI flags, new default adapters, schema changes). Needs its own validation pass — ideally a canary scenario built specifically from a real release diff — before `auto_upgrade_check` defaults on fleet-wide.
- **Risk:** Two producers can now write `~/.needle/bin/needle-testing` — the self-modification pipeline and the new supervisor-driven release check. Needs a simple mutual-exclusion rule (e.g., the supervisor's release check skips writing if a `:testing` binary is already present and unpromoted, so it never clobbers an in-flight self-modification candidate mid-validation).
- **Deferred, not required for v1:** automatic rollback triggered by post-promotion outcome-rate anomalies, using the existing `needle rollback` primitive.

## Evidence

- `src/upgrade/mod.rs`: `perform_upgrade()` (GitHub-release download, direct overwrite of `env::current_exe()`, no canary step) and `check_for_update()` (version comparison against GitHub Releases API) — read directly.
- `src/canary/mod.rs`: release-channel doc comment (`needle-testing` / `needle-stable` / `needle-stable.prev`), `CanaryRunner::testing_binary()` / `stable_binary()` / `promote()` — read directly; unit tests `promote_moves_testing_to_stable`, `promote_first_time_no_existing_stable`, `promote_no_testing_binary_fails` confirm the promotion mechanics are implemented and tested.
- `src/worker/mod.rs` (~lines 2260–2410): `check_auto_canary()` and `check_hot_reload()`, called every worker-loop cycle between LOGGING and SELECTING; gating on `self_modification.enabled` / `auto_promote` / `hot_reload` confirmed by direct read.
- `.needle.yaml` (this repo's live, self-hosted config): `self_modification: { enabled: false, auto_promote: false, hot_reload: true, ... }` — confirms the propagation half is armed (`hot_reload: true`) but the trigger half is not.
- Live version check, 2026-07-20, ex44 host: `needle --version` → `needle 0.2.11`; GitHub API `releases/latest` → `v0.2.12`, `published_at: 2026-07-20T12:49:30Z` (same day).
- `needle status` / `needle list` / `ps aux | grep needle` at time of audit: zero live NEEDLE-prefixed processes on ex44 (fleet activity elsewhere) — confirms the gap is structural (no path reacts to a new release), not merely "a worker happened to not check yet."

## Addendum (2026-08-12): acceptance re-verification — the drift direction inverted

Re-probed at acceptance, twenty-three days after the decision was drafted. The
problem this ADR exists to solve is still real, but it now points the other way,
and that changes one implementation detail materially.

**Then (2026-07-20, ex44):** installed `needle 0.2.11`, latest GitHub release
`v0.2.12`. The host lagged the release by one.

**Now (2026-08-12, ex44):** installed `needle 0.2.19`, latest GitHub release
`v0.2.16`. The host is **three releases ahead of anything published.**

So the failure mode is no longer "nobody ran `needle upgrade`" — it is that the
release pipeline stopped tracking what the fleet actually runs. Both are the same
underlying gap this ADR names (no automatic reconciliation between the release
channel and the installed artifact), and neither existing path notices either
direction.

**Design constraint this imposes, which the Decision as written does not state:**
`check_for_update()` must compare **strictly greater**, never merely different.
With today's state a "versions differ, fetch latest" implementation would
*downgrade* the fleet from 0.2.19 to 0.2.16 on the first poll, and — because the
Decision routes releases through `:testing` where `check_hot_reload()` will pick
them up — it would do so automatically, on every host, at the next canary
promotion. A downgrade is indistinguishable from an upgrade to every mechanism
downstream of the version comparison, so the comparison is the only place it can
be prevented. `supervisor.auto_upgrade_check` must also refuse to act when the
installed version is unknown or unparseable rather than treating that as "older".

**Other state confirmed at acceptance:**

- `supervisor.auto_upgrade_check` and `supervisor.update_check_interval_secs` do
  not exist in `src/` — this ADR remains entirely unimplemented.
- `.needle.yaml` still has `self_modification.enabled: false` and
  `auto_promote: false`, exactly as the Context describes, so the canary gate is
  still inert.
- **`hot_reload: true` is set and the channel is live**, which the Context did not
  state. `~/.needle/bin/needle-stable` exists, so `check_hot_reload()` runs its
  hash comparison on every worker cycle rather than short-circuiting on a missing
  `:stable`. It is currently quiescent only because the running binary and
  `needle-stable` are byte-identical (both `sha256:9ce8450c…`, 0.2.19). The
  re-exec path is therefore already load-bearing in production, and anything that
  writes `needle-stable` takes effect fleet-wide without a canary today.
- Promotion has been happening by hand, which is the strongest evidence for the
  decision: `~/.needle/bin/` holds `needle-stable.prev`,
  `needle-stable.pre-0.2.14-backup`, and `needle-stable.pre-assignee-fix.bak` —
  three hand-named rollback points from three manual interventions.

**Sequencing note.** ADR-013 (accepted the same day) makes Phase 16 authorized
work, and its staged-rollout item changes the claim path for every backend
including NEEDLE's own coordination substrate. This ADR is the mechanism that
makes that rollout survivable — canary-validate the artifact before it becomes
`:stable` — so the two should land in that order rather than in parallel.
