# Worker Lifecycle and Safe Cleanup

`needle run` launches a worker in a dedicated tmux session. The normal lifecycle
is:

1. `run` creates `needle-<agent>-<identifier>` and starts the worker inside it.
2. The worker registers its identity and heartbeat, then selects, claims, and
   dispatches beads.
3. Each outcome is recorded before the worker selects the next bead.
4. When the queue is empty, `worker.idle_action` decides whether it exits or
   waits for more work. The quickstart sets it to `exit`; fleet workers commonly
   use `wait`.
5. A clean exit removes the worker's live state. A tmux session or process that
   remains after the worker exits is stale and can be cleaned up.

The commands below operate on the local host and the state selected by the
current `HOME`. They do not coordinate workers on another host.

## Observe before changing anything

Use `list` to reconcile tmux sessions with the operating-system process table:

```bash
needle list
needle list --format json
```

`needle list` includes every discovered `needle run` process, including workers
that are no longer inside tmux. It labels a process as `non-tmux` when the
worker outlived its session, and reports stale sessions whose backing process
is gone. `needle status --by-worker` adds registry and worker-state information:

```bash
needle status --by-worker
```

These are read-only, host-wide views. A `--workspace` passed to `needle run`
sets that worker's home workspace; it is not a control-plane filter for `list`,
`status`, `stop`, or `cleanup`. The Explore strand may also discover other
workspaces unless its configured workspace list is explicitly fixed.

To watch one worker, use its identifier or a unique part of its session name:

```bash
needle attach alpha
# Detach without stopping it: Ctrl-b, then d
```

An ambiguous identifier is rejected. Check `needle list` and use a more
specific identifier when multiple sessions match.

## Scoped and global actions

The distinction between a deliberate target and a fleet-wide action matters:

| Command | Target | Effect and safety rule |
| --- | --- | --- |
| `needle stop -i alpha` | Sessions whose name contains `alpha` | Stops each matching worker process tree, then removes its tmux session. |
| `needle stop --all` | Every NEEDLE tmux session | Stops all visible workers. Use only when the whole local fleet is intended. |
| `needle cleanup` | Only sessions with no live pane process tree | Safe orphan cleanup; live sessions are preserved. |
| `needle cleanup -i alpha` | Sessions whose name contains `alpha` | Deliberate session removal; the identifier form bypasses the liveness check. |
| `needle cleanup --all` | Every NEEDLE tmux session | Destructive session removal, including sessions with live workers. |

`stop` requires either `-i` or `--all`; an unscoped stop is rejected. The
identifier is a session-name substring, so inspect the exact session name before
using it. `cleanup` with no flags is the only cleanup form that proves the
session's pane process tree is dead before removing it.

Cleaning up a session is not the same operation as stopping a worker. Removing
tmux can cause a still-running worker to be reparented, after which it can keep
claiming beads without a visible session. Always stop first, then clean up any
remaining dead session.

## The safe stop-and-clean workflow

For one worker:

```bash
# 1. Identify the exact session and PID.
needle list
needle status --by-worker

# 2. Stop the worker and its descendants.
needle stop -i alpha

# 3. Reconcile the process table. Do not assume the session result was enough.
needle list

# 4. Remove only sessions now proven to be orphaned.
needle cleanup
```

For a deliberate whole-host shutdown, replace the stop command with
`needle stop --all`, then run `needle list` and bare `needle cleanup`. Do not
replace either workflow with `needle cleanup --all` unless removing live tmux
sessions is explicitly intended.

The default stop grace period is 10 seconds and can be changed with
`stop.grace_period_secs` in the global config. A clean stop prints `Stopped:`.
If a signalled process remains after the grace period, `stop` prints the PID(s),
returns non-zero, and may still have removed the tmux session. Treat that as an
incomplete stop, not success.

## When the worker process is still live

This can happen after a tmux session is killed manually, after an explicit
cleanup, or when a worker ignores or cannot complete shutdown. The process is
not safe to forget just because `tmux ls` no longer shows a session.

1. Run `needle list`. A survivor should appear under discovered workers and
   will normally be marked `(non-tmux)`.
2. If `stop` reported a survivor, re-check the reported PID promptly with
   `ps -o pid,ppid,stat,cmd -p <pid>`. Confirm that it is still the intended
   NEEDLE process before sending any signal; PIDs can be reused.
3. Terminate the confirmed survivor using the host's normal process controls,
   escalating only if a graceful signal does not work. Do not use a broad
   pattern that could match another worker or an agent subprocess.
4. Run `needle list` again. Only after no worker process remains should you run
   `needle cleanup` or the intentionally scoped `needle cleanup -i <id>`.

If `needle list` discovers a worker with no registry entry, treat it as live
until the process table says otherwise. Registry cleanup is not proof that the
process stopped, and restarting the same identifier while the old process is
alive can create two workers competing for the same queue.

## Resume versus restart

`--resume` is the hot-reload path. It loads heartbeat and registry state for a
worker identity and continues from the worker loop without creating a new tmux
launcher session:

```bash
needle run --resume --identifier alpha
```

NEEDLE normally invokes this itself when replacing a worker binary. Operators
should use it only after `needle list` confirms that the previous process is
gone. If the old process is still live, stop and reconcile it first; starting a
resume process beside it defeats the identity and claim-safety assumptions.

For an ordinary operator restart, create a fresh managed tmux session instead:

```bash
needle run --agent claude --identifier alpha
```

This command must not be run until the old worker and any old session for
`alpha` have been reconciled. A normal `run` rejects an occupied identifier.

## Quick reference

```bash
needle list                         # sessions and all discovered processes
needle status --by-worker           # registry/state summary
needle attach alpha                 # observe one live session
needle stop -i alpha                # scoped process-tree stop
needle stop --all                   # global process-tree stop
needle cleanup                      # safe orphan-only session cleanup
needle cleanup -i alpha             # deliberate scoped session removal
needle cleanup --all                # destructive global session removal
needle run --resume -i alpha        # hot-reload resume, only after old PID is gone
```

For the disposable example, the [quickstart](../examples/quickstart/README.md)
shows this workflow with a sandboxed `HOME`. Do not run its cleanup commands
against a real fleet configuration.
