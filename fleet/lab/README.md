# lab bare-metal NEEDLE fleet — source-controlled policy

Owner for the lab (NixOS, 12 logical cores) NEEDLE worker fleet: the systemd
unit template, the resource slice, and the fleet membership manifest
(which workers exist, on which workspace, staggered by how much). Before
2026-09-10 all of this lived only in `~/.config` dotfiles on the host, so
"how many workers should lab run" had no answer anywhere in git.

Applied by [`apply-lab-fleet.sh`](apply-lab-fleet.sh); see that script for
what it deliberately never touches (credentials, systemd-managed drop-ins).

| File | Deploys to |
|---|---|
| `needle-worker@.service` | `~/.config/systemd/user/needle-worker@.service` |
| `needle.slice` | `~/.config/systemd/user/needle.slice` |
| `workers.tsv` | rendered to `~/.config/needle/workers/<identifier>.env` |
| `bin/cargo` | `~/.local/bin/cargo` (opt-in: `--install-cargo-wrapper`) |

## CPU budget accounting (needle-3d5c65d8, documented 2026-09-10)

Launch admission (`src/rate_limit/mod.rs`) reads
`std::thread::available_parallelism()` for its core count. That call is
cgroup-v2-quota-aware, so **a worker inside `needle.slice` sees the slice
quota, not the host** — confirmed in the 2026-09-03 journal, where every
admission abort printed `/ 7 cores` on a 12-core host.

| Layer | Value | Where set |
|---|---|---|
| Host logical cores (`nproc`) | 12 | hardware |
| `user.slice` (all user processes) | `cpu.max` = 9 cores | host admin |
| `needle.slice` (fleet workers + their dispatches) | `CPUQuota=700%` = **7 cores**; `MemoryHigh=24G`, `MemoryMax=32G`, `TasksMax=1500` | `needle.slice` + `needle.slice.d/{cpu,limits}.conf` (folded into the tracked fragment) |
| Core count admission divides by | **7** (slice quota), not 12 | `available_parallelism()` in `src/rate_limit/mod.rs` |
| Effective CPU admission threshold | `worker.cpu_load_warn` 0.80 × 7 = defer above 1-min load **5.6** | `worker.cpu_load_warn` (config, stays enabled) |
| Agent-spawned `cargo` scopes | `CPUQuota=200%`, `MemoryMax=6G` per scope, in `app.slice` — **outside** the 7-core fleet budget | `~/.local/bin/cargo` wrapper |

Two consequences worth internalizing before touching any of these numbers:

1. **Admission is deliberately stricter than the host.** It defers new
   launches at load 5.6 on a 12-core box because only ~7 of those cores are
   the fleet's; the other 5 belong to the operator, monitoring, and
   everything in `app.slice`.
2. **`app.slice` cargo scopes are the budget hole.** They escape
   `needle.slice`, are capped only by `user.slice` (9 cores), and until
   2026-09-10 had **no time bound**.

## 2026-09-03 incident — mechanism and remediation

Five hung SIGIL `cargo test` binaries (`stream_collect*` tests that never
return) sat in abandoned `systemd-run` scopes, each entitled to 200% CPU.
Together they consumed ~7 of the 9 `user.slice` cores and pinned the 1-minute
load at ~7.0 — exactly the admission threshold, so every worker launch was
deferred. The then-deployed binary *aborted* after 4 deferrals (~125s), and
systemd's `Restart=always` / `RestartSec=30` brought it straight back: each
cycle took ~155-185s, slow enough that `StartLimitBurst=5` in 300s never
tripped. Fifteen services bounced for hours — restart counters reached
130-355, with 68 admission failures in 15 minutes. Historical context:
needle-a7e125a8 (2026-07-29, 115 leaked claude processes, load 300+).

Remediation, in three layers:

1. **Product (N-T33, separate bead, already shipped):** admission-blocked
   workers now *hold resident* and resume selection when the host recovers,
   instead of exiting. Lab runs needle 0.6.0 (01ecf05, 2026-09-08), which has
   this. This is why the current fleet shows zero restarts under load.
2. **Unit policy (this directory):** `RestartSec` 30s → 120s, start limit
   300s/5 → 900s/3, so any future fast-exit loop fails *visibly* after three
   cycles instead of herding systemd all day. The CPU/RAM admission threshold
   itself is untouched and must stay enabled — no
   `NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK` anywhere in these units.
3. **Leak bound:** `bin/cargo` adds `RuntimeMaxSec=14400` to the cgroup
   scope, so an orphaned cargo invocation is reaped by systemd after 4h
   instead of never (the SIGIL orphans had run 14-24 days).

Fleet size: **7 workers** (`workers.tsv`), the 2026-09-09 right-size of the
15-unit fleet — one workspace family each, ~1 core per worker against the
7-core slice, start-staggered 15s → 150s.

## 2026-09-10 orphan cleanup record

Every sustained process was attributed from read-only evidence (process
ancestry, scope → cgroup mapping, `~/.needle` logs, workspace bead stores)
before anything was stopped. 31 abandoned `run-p*.scope` units, spanning
2026-08-16 → 2026-09-08, all in workspaces with no remaining worker:

- 13 claude-dispatch scopes (SIGIL ×10, domain-check ×3): 9 of their beads
  already **Closed**; 3 InProgress with dead assignees (`claude-code-glm-4.7-lab-s1`,
  `claude-code-glm-4.7-lab-drawrace` — no such units; beads untouched for
  15-24 days).
- 18 cargo-test scopes: the test binary outlived its cargo parent inside the
  scope; each pairs with one of the attempts above.

Removed 2026-09-10 02:23 UTC via `systemctl --user stop` (systemd is the
scopes' owner; the underlying beads remain claimable). Post-stop: 0 scopes,
0 hung test binaries, all 7 workers still `active/running`, load 0.59.

Deliberately preserved: the two long-lived `claude` processes under
`herdr server` (operator sessions, not NEEDLE-owned), `lab-health-collector`
(fleet monitoring), and all 7 live workers.

Found during attribution, left for the owning repos: SIGIL `bf-1aqf5` and
`sigil-96ef0d42`, domain-check `bf-toud6j` are InProgress with assignees that
no longer exist — Mend will release them if those workspaces rejoin the
fleet.

## Operational notes

- `workers.tsv` pins every worker with `NEEDLE_WS`; roaming (`--workspace`
  dropped) is disabled on purpose until the claude-governor queue is triaged
  (see the comment in the unit template).
- The unit's `ExecStart` is `~/.local/bin/needle`, but a running worker may
  re-exec into `~/.needle/bin/needle-stable` (upgrade channel;
  `src/supervisor/binary_freshness.rs` watches it). `ps` showing
  `needle-stable` under a `needle-worker@` unit is expected.
- Converge without interrupting attempts: `apply-lab-fleet.sh` only
  daemon-reloads. Restart an instance explicitly to pick up unit changes.
- Rollback: every replaced file is backed up on the host with a timestamp
  suffix, and this directory is git history.
- References to `bench` and `ex44` in inherited comments are historical;
  ex44 was decommissioned (see CLAUDE.md, "This box's identity").
