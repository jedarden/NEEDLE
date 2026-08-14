# Rolling a NEEDLE binary back

This procedure assumes NEEDLE itself is broken. It does not use `needle` for
any step, and no step depends on dispatching a bead — if the claim path is what
broke, dispatching is exactly what you cannot do.

Executed end to end on 2026-08-14 against a contained canary workspace while
the production fleet stayed on its existing binary. Measurements below are from
that run, not from reading the code.

## 1. Know which file the fleet actually runs

Two separate paths matter, and they are different files:

| Path | Role |
| --- | --- |
| `~/.local/bin/needle` | What `needle run` resolves off `$PATH`. **This is what the fleet launches.** |
| `~/.needle/bin/needle-stable` | What hot-reload compares the running binary against. `~/.needle/bin/needle` is a symlink to it. |

They are ordinarily byte-identical copies, but nothing enforces that. **Restore
both**, or you will roll back the fleet's launcher while hot-reload still points
at the bad build (or the reverse).

Confirm what you have before changing anything:

```bash
sha256sum ~/.local/bin/needle ~/.needle/bin/needle-stable
~/.local/bin/needle --version
```

Known-good fallbacks kept alongside `needle-stable` (verify with `--version`,
do not trust the filename):

```
needle-stable.prev                  0.2.19
needle-stable.pre-assignee-fix.bak  0.2.19
needle-stable.pre-0.2.14-backup     0.2.12
```

## 2. Stop the affected workers

**Do not use `needle stop --all`.** It only kills tmux sessions, orphaning the
`needle run` supervisors and any in-flight `claude --print` dispatch — and it
would hit every worker, not the ones you are rolling back.

Per worker, escalate. Measured on 2026-08-14, each earlier signal was ignored
and the supervisor only died at the last step:

```bash
tmux kill-session -t needle-<agent>-<identifier>   # session dies, supervisor does NOT
kill -INT  <supervisor-pid>                        # ignored
kill -TERM <supervisor-pid>                        # ignored
kill -KILL <supervisor-pid>                        # required
```

Find the pids without NEEDLE:

```bash
ps -eo pid,cmd --no-headers | grep -E '^\s*[0-9]+ .*/needle run --workspace'
```

Verify the supervisor is gone before continuing. A surviving supervisor will
keep claiming with the binary you are trying to retire, and because the tmux
session is already gone you will not see it.

## 3. Restore the binary in both locations

```bash
cp ~/.needle/bin/needle-stable.prev ~/.needle/bin/needle-stable
cp ~/.needle/bin/needle-stable      ~/.local/bin/needle
sha256sum ~/.local/bin/needle ~/.needle/bin/needle-stable   # must match
~/.local/bin/needle --version                              # must be the expected version
```

Copy, do not `mv` a file that a running process may still hold: an unlinked
binary makes `/proc/self/exe` report `... (deleted)`, which hot-reload treats as
a forced re-exec (`HotReloadCheck::CurrentBinaryDeleted`).

## 4. Relaunch from an explicit path

Never rely on `$PATH` during a rollback — that is how you re-launch the binary
you just retired.

```bash
/home/coding/.needle/bin/needle-stable run \
  --workspace <repo> --agent <adapter> --count 1 \
  --identifier <name> --timeout 3600 --hot-reload false
```

For durable configuration, set `worker.worker_binary_path` in `.needle.yaml`.
That field exists precisely so worker spawning stops resolving
`Command::new("needle")` off `$PATH`.

## 5. Verify with real work, not with a version string

`--version` proves which file you launched, not that the claim path works.
Confirm a worker can actually claim and close:

```bash
bf ready                 # something claimable exists
bf show <bead-id>        # -> in_progress, assignee is your worker
bf show <bead-id>        # -> closed, with a close reason
```

If the bead reaches `in_progress`, the claim path is working — that alone
clears the failure mode this procedure exists for.

## Notes

- The canary and the fleet can run different NEEDLE versions at the same time.
  Nothing reconciles them today, because `Worker::check_hot_reload` is never
  called (`#[allow(dead_code)]`) and `self_modification.enabled` is `false`.
  **If hot-reload is ever re-enabled this stops being true**: it compares the
  running binary's *hash* against `needle-stable` and re-execs into
  `needle-stable` on any difference — hashes, not versions — so a worker
  deliberately pinned to a newer build gets dragged back.
- Backend coexistence does not need two binaries. One binary carries both the
  `bead-rs` and `bead-forge` descriptors and selects per workspace from
  `.needle.yaml`'s `bead_cli.backend`. Isolate backends per repository, and
  reserve binary staging for the binary itself.
