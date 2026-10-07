# File-Contention Hook Contract (`file_contention_hook/v1`)

**Status:** published contract, plan Phase 20 (needle-7f04ec50).
**Implementation:** `src/file_contention/`, `src/cli/file_contention.rs`,
worker bracket in `src/worker/mod.rs` (`begin_file_contention` /
`end_file_contention`).

## What this is — and is not

An **optional**, **advisory** protocol that lets agents sharing **one
checkout** say "I am about to write this file" before they write it, so a
second participating agent backs off instead of clobbering work in progress.

- It only coordinates **participating** agents in the **same checkout**.
  A harness that does not run the hook, a human editor, and a separate clone
  or worktree are all invisible to it. Separate checkouts never see each
  other's markers.
- It never replaces bead-rs claims, fencing, resource keys, or dependencies.
  Those decide *who works a bead*; markers only warn about *files*.
- A marker is a warning, never a lock: nothing stops a write. A hook that
  ignores a conflict result writes anyway.
- Nothing here is enabled by default. Coverage needs **both** a capable
  adapter **and** a workspace that opts in.

## Capability advertisement

A harness adapter advertises support in its adapter YAML:

```yaml
capabilities:
  - file_contention_hook/v1
```

Advertise it **only** when the harness really runs a pre-write hook before
every create, edit, delete, and rename it performs, and honours the results
below. Do not add it to a harness that lacks a pre-write lifecycle. No
built-in adapter advertises it today. `tests/fixtures/file-contention-hook-adapter.yaml`
is a test fixture, not a harness.

A workspace opts in through its own `.needle.yaml`:

```yaml
file_contention:
  enabled: true      # default false
  lease_secs: 120    # default 120, minimum 30
```

## Coverage states

Resolved per dispatch from the adapter's capabilities and the **bead's**
workspace (not the worker's home workspace):

| Coverage | When | Effect on dispatch |
|---|---|---|
| `enabled` | capable adapter + workspace `enabled: true` | contract env added; `.needle/locks/` excluded from git |
| `supported_disabled` | capable adapter, workspace not opted in | none — dispatch unchanged, no marker tree |
| `unsupported` | adapter does not advertise the capability | none — dispatch unchanged, no marker tree |
| `degraded` | opted in, but markers cannot be made safe (invalid config, git exclude failed) | none — reported, never treated as protected |

Telemetry `file_contention.coverage` records every non-`unsupported`
resolution.

## Inputs: the dispatch environment

Under `enabled` coverage only, the agent process receives:

| Variable | Meaning |
|---|---|
| `NEEDLE_FILE_CONTENTION` | `enabled`. Hooks act **only** on this value. |
| `NEEDLE_FILE_CONTENTION_CONTRACT` | `file_contention_hook/v1` |
| `NEEDLE_FILE_CONTENTION_REPO` | repository root whose `.needle/locks/` holds markers |
| `NEEDLE_FILE_CONTENTION_LEASE_SECS` | lease in seconds |
| `NEEDLE_FILE_CONTENTION_PID` | holder pid used for liveness (the dispatching worker) |
| `NEEDLE_BEAD_ID` | bead being worked |
| `NEEDLE_ATTEMPT_ID` | attempt that owns the markers |
| `NEEDLE_WORKER_ID` | worker identity |

The dispatcher strips any **inherited** `NEEDLE_FILE_CONTENTION*` variable
that this dispatch did not set itself, so a nested dispatch never acts on an
enclosing attempt's contract.

An interactive agent (no NEEDLE dispatch) participates by setting
`NEEDLE_SESSION_ID` (or passing `--session`) instead of an attempt. Its
`--worker` defaults to `interactive`, and its holder pid defaults to the
parent of `needle contention`. Pass `--pid` with a long-lived process when
the hook runs through a short-lived shell.

## Request: `needle contention`

Every flag falls back to the variable above, so a hook needs only the paths.

| Hook moment | Command |
|---|---|
| before creating a file | `needle contention acquire --intent create --path P` |
| before editing a file | `needle contention acquire --path P` (`--intent modify` is the default) |
| before deleting a file | `needle contention acquire --intent delete --path P` |
| before a rename | `needle contention acquire --rename-from OLD --rename-to NEW` |
| several files in one operation | repeat `--path`; acquisition is **all-or-none** |
| later writes to a held file | the same `acquire` again — the holder renews instead of conflicting |
| inspect without recording | `needle contention check --path P` |
| see everything in the checkout | `needle contention list` |
| finished early | `needle contention release [--path P]` |

Paths are repo-relative or absolute inside the repo. A path escaping the
repo (`..`, an outside absolute path, or a symlink escape) and anything under
`.git/` or `.needle/` is rejected.

Acquire lazily, on the **first** write intent for a path, never up front for
a whole plan. A rename records both source and destination, or neither.

## Response: exit code and `--json`

| Exit | Result | The hook must |
|---|---|---|
| `0` | acquired / ok / disabled | proceed with the write |
| `2` | invalid request | treat as a hook bug; do not assume protection |
| `3` | another participant is active on a path | **not write**; back off, pick other work, or report the holder |
| `4` | needs attention: stale-modified, closed-but-modified, or ambiguous marker | **not overwrite**; preserve the file and route it (resume via the named bead, or file a follow-up inspection) |
| `5` | degraded: the marker tree is unusable | proceed only as an **uncovered** write and say so; never report it as protected |

With `--json`, stdout carries one document with `"schema":
"needle.file_contention.v1"`, a `result`, the workspace `coverage`, and, per
path, its `assessment` and `holder` (bead, attempt or session, worker, host,
pid, intent, `claimed_at`, `last_renewed_at`, `lease_expires_at`, `age_secs`).
No prompt or file content is ever included.

Assessments:

| Assessment | Meaning | Exit |
|---|---|---|
| `free` | no marker | 0 |
| `same_attempt` | the caller already holds it | 0 |
| `active_other` | a live holder (past its lease is a **warning**, not loss of authority) | 3 |
| `stale_unchanged` | holder dead or expired and the file matches its baseline — taken over atomically by `acquire` | 0 |
| `verified_closed` | owning bead closed; file unchanged is reclaimable, modified needs attention | 0 / 4 |
| `stale_modified` | holder gone but the file changed since the claim — never taken over | 4 |
| `ambiguous` | unreadable, contradictory, or unverifiable marker | 4 |

A timestamp alone never proves modification: the baseline compares
existence, size, and content hash.

## Cleanup

- The worker releases every marker the attempt owns on **every** terminal
  outcome (`do_log`) and on shutdown release, even when the claim itself
  cannot be released. Telemetry: `file_contention.released`.
- Markers the attempt never released (crash, kill) expire through liveness
  and lease, then are reclaimed on the next `acquire` if the file is
  unchanged. They are preserved if it changed.
- A hook should `release` paths it abandons early. It must not remove
  another participant's marker. `release` only ever removes the caller's own.

## Git safety

`.needle/locks/` is checkout-local and must never be committed. NEEDLE adds
`/.needle/locks/` to the repository's local `info/exclude` (never to
`.gitignore`) before the first marker. Diffs, dirty-path scans, and
shipped-work checks exclude it. The commit guard and the definition of done
reject a staged marker, including a `git add -f`. The worker reports any
marker that reached a commit during the attempt.

## Not covered

Writes a hook cannot see — arbitrary shell commands (`sed -i`, build output,
code generators), editors outside the harness, other checkouts — are
**uncovered**. A harness must report them as uncovered rather than imply
markers protected them.

## Versioning

`file_contention_hook/v1` names this document. Renaming a variable, changing
an exit code, or changing the JSON schema is a breaking change that needs
`v2` and a new capability string. Adding optional JSON fields is not.
