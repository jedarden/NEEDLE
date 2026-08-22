# NEEDLE Codex Guide

This file is the primary operating guide for Codex in this repository, both in
interactive sessions and when Codex runs headlessly as a NEEDLE worker. Inspect
the current repository state and the relevant source before relying on older
examples or planning documents.

## Project Overview

NEEDLE (Navigates Every Enqueued Deliverable, Logs Effort) is a Rust worker
binary. It selects and claims work through the repository's configured bead
backend, dispatches a coding agent, records telemetry, and routes every outcome
through an explicit state machine.

The primary backend is bead-rs (`bead`); bead-forge (`bf`) remains supported
for explicitly bound legacy workspaces. Check `.needle.yaml` and the `.beads/`
layout before running either CLI. Do not infer the backend from which binaries
happen to be installed, and never use one backend's repair or import command on
the other backend's database. `br` is retired and must not be introduced in
source, prompts, scripts, tests, or documentation.

## Working Safely

- Inspect `git status --short` before editing. Preserve unrelated user and
  worker changes in a dirty worktree.
- Multiple NEEDLE workers may share the same repository. Re-check files before
  modifying them and avoid broad rewrites that can overwrite concurrent work.
- **Do not create per-worker git worktrees to isolate concurrent workers** —
  a shared checkout is the intended model, not a bug to route around.
  Worktrees add disk and build-cache duplication without addressing the
  actual failure mode, which happens at the bead level: the same bead getting
  claimed and worked twice (confirmed 2026-08-09, commitgraph `cg-l0v0kc` —
  two byte-identical commits at the same second from two concurrent workers).
  The supported backends provide atomic claim operations, so avoid this by
  decomposing and dependency-ordering beads so the same unit of work is never
  independently claimable twice — see the target repo's own `AGENTS.md` for
  repo-specific guidance.
- Do not use destructive Git operations (`reset --hard`, forced checkout,
  forced push) to resolve unrelated changes.
- Forgejo (`git.ardenone.com`) is the authoritative remote. GitHub is a mirror.
  Push only to the configured `origin`, and never use `--force` or
  `--force-with-lease`.
- Keep commands suitable for unattended execution: avoid prompts, pagers, and
  commands that require an interactive terminal.
- Treat `docs/plan/plan.md` as architecture and design history. Verify current
  behavior in source and against the installed CLI before depending on old
  command examples.

## Rust Compatibility

The declared MSRV is Rust 1.75 (`Cargo.toml`). Do not add language features or
dependencies that require a newer compiler without intentionally updating the
MSRV and associated toolchain and CI configuration.

## Code Conventions

- Prefer `Result` and `?` for fallible operations. Do not hide operational
  failures with `unwrap()` or `expect()`. Reserve those calls for tests or a
  clearly documented invariant/unrecoverable initialization condition.
- Fallible public operations should return `Result`; infallible constructors,
  accessors, and pure transformations may return ordinary values.
- Match state and outcome enums exhaustively. Avoid catch-all `_` arms where a
  new variant should force an explicit decision.
- Emit telemetry for every state transition and terminal outcome.
- Preserve error context at process, filesystem, database, and parsing
  boundaries.
- Unit tests belong in `#[cfg(test)]` modules near their implementation.
- Integration and end-to-end tests belong under `tests/`.
- Use `#[tokio::test]` for asynchronous tests; do not introduce
  `tokio_test::block_on`.
- Prefer testing public behavior over implementation details.

## Critical Test Isolation

Any test that spawns the compiled `needle` binary as a real subprocess (for
example, `Command::new(CARGO_BIN_EXE_needle)`) must isolate both `HOME` and the
Explore strand's scan root.

Explore is enabled by default and otherwise scans under the real home
directory. An unisolated test can discover and mutate production bead stores.
This previously created hundreds of phantom beads across real repositories.

At minimum, give the subprocess a temporary home:

```rust
cmd.env("HOME", temp_dir.path());
```

Prefer also configuring `workspace_root` to a temporary fixture directory, or
disable Explore when the behavior under test does not require it. Never point a
test worker at `/home/coding` or another directory containing real projects.

## Verification

Your work is complete when `scripts/definition-of-done.sh --fast` passes with zero failures.

Before closing any bead:
1. Run `scripts/definition-of-done.sh --fast`
2. If it fails, address ALL reported issues (not just the first one)
3. Re-run until it passes
4. Only then close the bead

This ensures the same verification that runs in pre-commit hooks and CI gates.

### Definition of Done

NEEDLE uses a unified definition-of-done system invoked identically by:
- **Pre-commit hook**: `scripts/definition-of-done.sh --fast --count-bypass`
- **NEEDLE validation gate**: `scripts/definition-of-done.sh --fast` (configured in `.needle.yaml`)
- **CI verify step**: `scripts/definition-of-done.sh --all` (runs both lanes)
- **Agent completion**: `scripts/definition-of-done.sh --fast` (run before closing beads)

This single source of truth prevents drift between surfaces and ensures "what is the agent held to?" has exactly one answer.

### Verification Lanes

The unified script splits checks by COST, not by tool:

**Fast lane** (seconds, runs locally under cgroup):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo check`

**Slow lane** (tests, may submit to iad-ci):
- `cargo test --lib` (unit tests)
- `cargo test --test integration_tests` (core integration tests)
- `cargo test --test p2_integration_tests` (Pluck, Mend, Explore, and Knot)
- `cargo test --test p3_integration_tests` (Weave, Unravel, and Pulse)
- `cargo test --test real_br_integration_tests` (real bead-rs strand backend)

**Full verification** (CI runs on push to main):
- Both fast and slow lanes together

### Failure Aggregation

The unified script aggregates ALL failures rather than aborting on the first. This prevents wasted cycles where an agent fixes fmt, gets re-dispatched, discovers clippy, and has to repeat. Each run learns everything at once.

### Bypass Counting

Pre-commit bypasses are recorded in `.beads/bypasses.jsonl`. Each `git commit --no-verify` increments the count, making invisible bypasses impossible. Monitor this file to detect when quality gates are being skipped.

### On this Host

The `cargo` wrapper may offload a clean repository to iad-ci and use a resource-limited local fallback for a dirty repository. Do not assume a command ran remotely; report what actually ran and its result.

The authoritative full verification is the `needle-ci` workflow on iad-ci after a push to `main`. Do not claim full-suite success from the fast lane alone.

If CI fails, record the failure on the bead, fix it, and do not close the bead as successfully completed.

## Bead Workflow

Each bead supplies its own deliverables and acceptance criteria. Complete and
verify the requested repository work before closing it.

This repository is bead-rs-backed. Use:

```bash
bead close BEAD_ID --reason "Summary of what was done"
```

The close flag is `--reason`, not `--body`. When no code change is appropriate,
record the reason with a bead comment or another supported public `bead`
operation rather than creating an empty commit.

SQLite (`.beads/beads.db`) is the live store. `.beads/checkpoint/` is the
git-tracked durable checkpoint, and mutations do not flush it implicitly.

Flush explicitly before committing bead state:

```bash
bead sync flush-only
./scripts/checkpoint-publish.sh stage
```

The staging helper resolves and verifies the generation objects named by both
checkpoint pointers, stages those objects atomically with the pointers, and
removes superseded `objects/gen-*.jsonl` files from the working tree. Install
the tracked pre-commit check once per clone with
`./scripts/install-git-hooks.sh`. Do not use `git add -A` for checkpoint
publication.

`bead doctor` is read-only by default. If a database is missing, wrong-schema,
or corrupt, confirm the backend first and restore an empty native store from
the committed forensic checkpoint with the documented `bead init` plus `bead
sync import-only --restore-into-empty` procedure. Never delete or rebuild a
bead database without explicitly accounting for unflushed work.

## Commits

Use the bead identifier when the work is bead-driven:

```text
feat(needle-XYZ): short description
fix(needle-XYZ): short description
test(needle-XYZ): short description
```

Do not mix unrelated cleanup into the task commit. State which checks were run
and any remaining limitations in the final handoff or bead notes.

## Telemetry Contract

NEEDLE emits OpenTelemetry-compatible telemetry via OTLP. When modifying code that interacts with the telemetry system, maintain these semantic conventions:

### GenAI Attributes

The `agent.dispatch` span uses OpenTelemetry's `gen_ai.*` semantic conventions:

| Attribute | Description |
|-----------|-------------|
| `gen_ai.system` | AI provider (e.g., `anthropic`, `openai`) |
| `gen_ai.request.model` | Model identifier (e.g., `claude-sonnet-4-6`) |
| `gen_ai.usage.input_tokens` | Input token count |
| `gen_ai.usage.output_tokens` | Output token count |

These attributes enable NEEDLE telemetry to integrate with GenAI-focused dashboards (Grafana GenAI app, Langfuse, Honeycomb AI, etc.).

### Resource Attributes

Every exported signal carries these resource attributes:

| Attribute | Value |
|-----------|-------|
| `service.name` | `"needle"` |
| `service.version` | Build version from `CARGO_PKG_VERSION` |
| `service.instance.id` | Worker ID (e.g., `needle-claude-anthropic-sonnet-alpha`) |
| `needle.session_id` | Per-boot random session ID |
| `host.name` | Hostname |
| `process.pid` | Worker PID |

For the complete semantic mapping of NEEDLE events to OpenTelemetry signals, see [`docs/plan/plan.md`](docs/plan/plan.md).
