# ADR-014: Explicit Workspace Binding for Bead Backends

**Status:** Accepted — 2026-08-12
**Deciders:** operator (jedarden), via Codex
**Supersedes:** ADR-013 §7 only where it specifies ordered `auto` detection
**Tracking:** Phase 16 in `docs/plan/plan.md`; beads use label `bead-cli-backend`

## Context

ADR-013 correctly makes bead CLIs descriptor-driven, but its default-selection
rule is unsafe. It says `auto` tries `bead`, then `bf`, then `br`. Binary
availability and operator preference do not establish which tool owns a
workspace. Fleet hosts commonly install more than one CLI, and bead-rs and
bead-forge both use a `.beads/` directory containing SQLite state. Selecting
the first executable on `PATH` can therefore run one implementation against
another implementation's live database.

The transition also needs to be reversible per repository. Existing
bead-forge repositories must remain on bead-forge while selected repositories
move to bead-rs. A host-wide default cannot express that boundary, and an
implicit probe makes the result change when packages or `PATH` change.

## Decision

Every workspace operated by a NEEDLE worker declares its bead backend in the
repository's `.needle.yaml`:

```yaml
bead_cli:
  backend: bead-forge
```

or:

```yaml
bead_cli:
  backend: bead-rs
```

The value names a loaded `BeadBackend` descriptor, not merely an executable.
An optional explicit binary path remains a host/operator override, but durable
repository configuration should normally name only the backend so paths can
differ across machines.

The binding has these invariants:

1. **Repository configuration is authoritative.** Worker, supervisor, strands,
   validation, prompts, recovery, and doctor all receive the same resolved
   backend. No consumer performs its own `which` chain.
2. **Identity is verified before store access.** The selected descriptor's
   `identity_pattern` must match the configured/resolved binary's version
   output before any command that may open or mutate `.beads/` runs.
3. **Missing or contradictory ownership fails closed.** A worker does not infer
   ownership from installed binaries, directory names, SQLite tables, or the
   presence of `.beads/`. It reports an actionable configuration error.
4. **`auto` is discovery assistance, not worker authority.** Doctor/onboarding
   may inspect candidates and propose a binding. They must not write it without
   an explicit operator command, and a production worker never dispatches from
   an uncommitted guess.
5. **Bindings are workspace-scoped.** Explore and other cross-workspace strands
   load the target workspace's binding independently. The home workspace's
   backend does not leak into a remote workspace.
6. **Changing the binding does not migrate data.** It is a routing change, not
   a checkpoint conversion. The destination workspace must already be created
   and reconciled through the migration/rehydration procedure appropriate to
   the selected tools.

### Transition policy

Before enforcement is enabled, an audit enumerates every discovered workspace,
the configured binding (if any), candidate binaries, and whether identity
verification passes. Existing bead-forge repositories receive an explicit
`bead-forge` binding through normal reviewed repository changes. Selected new
or rehydrated workspaces receive `bead-rs` only after their native store and
recovery checkpoint have been verified.

The fleet rollout must not silently insert bindings. During a bounded warning
period, an unbound workspace may be reported as a legacy bead-forge candidate,
but it remains ineligible for dispatch. Removing that warning later is a
documentation/configuration cleanup, not a behavior change.

## Consequences

- Installing or upgrading `bead`, `bf`, or `br` cannot switch a repository's
  backend.
- A fleet can operate bead-forge and bead-rs repositories simultaneously.
- New repositories require one explicit configuration choice before dispatch.
- Operators get an extra onboarding step, in exchange for preventing a CLI
  from opening another implementation's live database.
- ADR-013's descriptors, strategies, capability reconciliation, and identity
  checks remain unchanged. Only ordered `auto` selection as worker authority
  is rejected.

## Rejected alternatives

- **First executable on `PATH`.** Host state is not workspace ownership.
- **Prefer the primary backend globally.** Product priority does not migrate
  existing repositories.
- **Infer ownership from SQLite schema.** Detection requires opening the very
  database that must be protected, couples NEEDLE to private schemas, and may
  become ambiguous as implementations evolve.
- **Write a marker inside `.beads/`.** That makes NEEDLE mutate another tool's
  state namespace and creates a second authority beside repository config.
- **Use the home workspace backend for all Explore targets.** A fleet must be
  able to span repositories using different backends.

## Acceptance evidence required

- With `bead`, `bf`, and `br` all installed, two fixture repositories bound to
  different backends invoke only their configured binary.
- Missing, unknown, identity-mismatched, and contradictory bindings fail before
  any store command is spawned.
- Explore resolves two differently bound workspaces independently in one scan.
- A real bead-rs lifecycle and an unchanged bead-forge lifecycle both pass the
  staged rollout gate.
