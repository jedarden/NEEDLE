# Close-Evidence Verification

Every close reason must carry evidence, and NEEDLE re-runs it. When an agent
closes a bead and exits successfully, the outcome handler
(`outcome::handle_success`) refuses to honour the close until the commands the
agent claimed — and the commands the bead itself names — have been re-run from
a clean extraction of the workspace's committed state.

This is an outcome-time gate, alongside shipped-work and the DoD-bypass check.
It closes the last unverified gap in the loop: workspace gates judge what the
workspace *declares*, shipped-work judges that *something* landed upstream —
but nothing checked that the agent's own "I ran the tests" claim was true, or
that the tests a bead names in its description were ever run at all.

## The `verified:` block

The pluck prompt (`src/prompt/mod.rs`, the "Close the bead" block) requires a
close reason to end with a fenced block whose info string is `verified:` (or
`verified`), listing one command per line with the exit code it produced:

````
Implemented the parser fix.

```verified:
go test -short ./internal/crypto/ exit=0
cargo test --lib exit=0
```
````

The claimed exit codes are informational only and are never trusted: every
command is re-run, which is the entire point of the block.

A close reason without the block is rejected: the bead is reopened and
released with reason `close reason carries no verification evidence`, and the
attempt counts toward the failure ceiling like any other gate failure.

## Re-run policy

`outcome::close_verification` parses the block and applies three rules.

**Allow-list.** Only these command prefixes are ever executed:

```
go test   go vet   go build
cargo test   cargo build
npm test   pytest   make test
scripts/definition-of-done.sh
```

The match is on the command's first tokens, so `cargo test --lib` qualifies
while `cargo testr`, `sudo cargo test`, or `scripts/definition-of-done.sh.bak`
do not. Anything outside the list is dropped with a WARN and **never
spawned** — a close reason is not a shell prompt, and the verifier must not
become an execution oracle for arbitrary text. The block still counts as
evidence once at least one allow-listed command re-runs.

**Acceptance commands.** The bead's own description is scanned for `go test …`
and `cargo test …` commands quoted in fenced code blocks or inline code spans,
and those are re-run too, merged after the claimed commands. An agent cannot
omit the test its bead names. Bare prose is deliberately not scanned: a
description line like "cargo test passes (push to main and wait for CI)" is
not a command, and neither is a template — a span containing an ellipsis
(`go test …`) names a family of commands, not one runnable command, so it is
never re-run.

Mined commands also re-run only where they can apply: `go test` needs a
`go.mod`/`go.work` in the workspace and `cargo test` a `Cargo.toml` — the
same build-marker logic the default gates use to pick a language. A Rust
workspace whose bead quotes `go test ./...` in prose gets no re-run, exactly
as its gates would not run a Go vet on it; without this scope, every prompt
template that cites example commands would fail the closes of the beads
quoting it. Claimed commands are deliberately **not** scoped this way: an
agent claiming `go test` ran in a Rust repo is claiming something false, and
the re-run exists to catch that.

Quote acceptance commands in backticks when authoring beads
(see [bead-authoring.md](bead-authoring.md)).

**Clean extraction.** Re-runs execute inside a fresh extraction of the
workspace's committed HEAD (`git archive`, the same mechanism as
`run_in: clean` command gates — see [definition-of-done.md]
(definition-of-done.md) for the gate-side contract). The shared checkout's
uncommitted state — including other workers' in-flight edits — cannot make a
re-run pass or fail. On success the extraction is removed; on failure it is
left in place for diagnosis.

## Verdicts

| Outcome | Handling |
|---|---|
| All re-runs exit 0 (or nothing was re-runnable) | Close honoured, normal success flow resumes |
| A re-run exits non-zero | Gate failure: reopen + release; the reason names the command and its first 20 lines of output |
| No `verified:` block | Gate failure: reopen + release, reason `close reason carries no verification evidence` |
| Extraction cannot be built, or a command could not spawn | Execution error: release **without** a failure-count penalty (ADR-023) — no verdict exists |

## Backend scope

The gate applies only where the bead store can expose the close reason at all.
bead-rs projects `close_reason` through its safe query language and is fully
supported. The legacy bf `show --json` projection carries no close reason, so
on those workspaces the gate skips (fail-open) rather than judging every close
as evidence-free. Backends advertise this through
`BeadStore::exposes_close_reason`; a *fetch error* on a capable backend also
fails open — a store hiccup is not missing evidence.

## Telemetry

Rejections and re-run failures flow through the existing gate-failure path:
`verification.failed` events carry the gate name `close_verification` and the
full rejection reason, and failure counts, quarantine, and the gate-health
fingerprint window all apply unchanged.
