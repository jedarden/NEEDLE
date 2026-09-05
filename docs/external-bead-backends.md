# External bead backends

NEEDLE can bind a workspace to an operator-installed bead CLI descriptor. The
workspace selects the descriptor by name; it cannot choose the descriptor
directory or substitute command templates.

## Install and select a descriptor

Place one YAML descriptor in:

```text
~/.config/needle/bead-backends/<name>.yaml
```

Then select its `name` in the workspace's committed `.needle.yaml`:

```yaml
bead_cli:
  backend: example-remote
```

An operator may set `bead_cli.path` to an explicit executable path. Otherwise
NEEDLE resolves the descriptor's `binary` on `PATH`, then its `detect_paths`.
The descriptor and resolved executable become one immutable runtime binding.
Changing `bead_cli.backend`, `bead_cli.path`, or a descriptor requires a worker
restart; a running worker does not silently switch stores.

Descriptors are trusted operator configuration. Do not install descriptors
from a repository under work or construct their operations from work-item
content. Two operator files with the same descriptor name are rejected as
ambiguous. A user file may intentionally replace a shipped built-in by name.

## Contract

External descriptors currently implement the same complete operation set as a
shipped backend. Validation rejects missing operations, unsupported strategy
names, invalid regular expressions, and unresolvable placeholders before any
store command runs.

NEEDLE executes `version_command` first, with a five-second timeout and bounded
stdout/stderr. Its output must match `identity_pattern`. A failed, timed-out, or
mismatched identity check prevents the store from opening. Native bead-rs
capability probing remains specific to the `bead-rs` descriptor.

Use `strategy: atomic_command` for a descriptor whose `claim` operation performs
one atomic claim. The command returns one JSON object:

```json
{"outcome":"claimed","bead_id":"work-123"}
{"outcome":"race_lost","claimed_by":"worker-b"}
{"outcome":"not_claimable","reason":"paused"}
{"outcome":"error","reason":"store unavailable"}
```

Malformed JSON, unknown outcomes, command failures, and missing required fields
are errors. They are never interpreted as an empty queue. Arguments are passed
as an argv vector, so IDs and actor names containing spaces or punctuation are
not shell-split.

Prompt fragments and canary lookups render from the same resolved descriptor.
The worker, supervisor, doctor, validation commands, and cross-workspace lookup
all use explicit workspace bindings rather than rediscovering a CLI.

## Local example

The hermetic fixture is executable without credentials or network access:

```sh
cargo test --test external_backend_runtime
```

It creates a temporary operator descriptor directory and fake CLI, selects the
external backend through normal configuration, verifies identity before store
mutation, exercises descriptor-rendered claim/release commands, and races two
claims to prove exactly one winner.

## Current boundary

This extension drives CLI-shaped stores. It does not yet add opaque remote lease
context, renewal, fenced terminal mutations, or capability-aware subsets of the
operation contract. Those require the separate remote-lifecycle change; an
external descriptor must not emulate them by pretending to be bead-rs.
