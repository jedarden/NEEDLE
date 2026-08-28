# ADR-021: Removal of bead-forge (bf) Backend Support

**Status:** Accepted — 2026-08-28
**Deciders:** operator (jedarden), via Claude Code
**Supersedes:** ADR-013 §7 (backend priority table) and ADR-013's rejection of "Standardize on one CLI and drop the others"
**Tracking:** beads under labels `bead-cli-backend` and `bf-removal`; Phase 16 completion

## Context

ADR-013 (2026-08-12) established a pluggable backend system supporting three bead CLIs simultaneously, with bead-rs as primary, bead-forge as secondary, and beads_rust as tertiary. At that time, **bead-forge (bf) was live and load-bearing** across the fleet:

- ADR-013 §7 established backend priority: bead-rs primary, bead-forge secondary, beads_rust tertiary
- ADR-013's "Alternatives Considered" section explicitly **rejected** "Standardize on one CLI and drop the others" on the grounds that "all three upstreams are live and independently maintained"
- Fleet hosts had both `bead` and `bf` installed, and existing workspaces used bead-forge stores

That rejection was correct for 2026-08-12. ADR-014 (2026-08-12) then required explicit per-workspace backend binding via `.needle.yaml`, enabling a gradual migration. Between 2026-08-12 and 2026-08-28, the migration completed:

1. **All workspaces migrated to bead-rs** — every `.needle.yaml` now declares `backend: bead-rs`
2. **bf uninstalled from fleet hosts** — `bf` is no longer in `PATH` on ex44 or lab
3. **bead-forge backend code removed from NEEDLE** — `BfCliBeadStore`, `bf`-specific resolution chains, and related prompts deleted
4. **No bead-forge backend declarations remain** — no `.needle.yaml` declares `backend: bead-forge` or `backend: bf`

The rationale for retaining multi-backend support — that multiple backends were "live and independently maintained" — no longer holds. One backend (bead-forge) is now retired and uninstalled. Continuing to support it in NEEDLE would be dead code.

## Decision

**Remove bead-forge (bf) backend support from NEEDLE entirely.**

NEEDLE now supports exactly one bead CLI: `bead` (bead-rs). The pluggable backend infrastructure remains (a future fourth backend could still be added as YAML), but the `bead-forge` descriptor and all bf-specific code are removed.

### Scope of removal

**Deleted from NEEDLE codebase:**
- `BfCliBeadStore` implementation (bead-forge-specific store logic)
- `bead-forge` builtin descriptor from `builtin_bead_backends()`
- `bf`-specific resolution chains (the five hardcoded `which bf` sites identified in ADR-013 §1)
- `bf`-specific prompt templates in `src/prompt/mod.rs`
- `bf`-specific version handshake and compatibility workarounds (bf 0.2.0 `--limit 0` workaround)
- Any test fixtures or mocks specific to bead-forge dialect

**Preserved:**
- The `BeadBackend` descriptor system itself (still extensible for future backends)
- The `bead-rs` descriptor (now the sole builtin)
- `CliBeadStore` engine (strategy enums, capability negotiation, identity verification)
- ADR-014's per-workspace binding invariant (`bead_cli.backend` in `.needle.yaml`)

**Configuration impact:**
- `.needle.yaml` files must declare `bead_cli.backend: bead-rs` (already true for all workspaces)
- `auto` detection now resolves only to `bead` — the `bf` fallback path is removed
- Missing or unknown `bead_cli.backend` values fail closed (as before)

### Transition verification

Before this removal, the following was verified on 2026-08-28:

1. **No active bead-forge backends in configuration:**
   - Searched all `.needle.yaml` files on ex44: zero declarations of `backend: bead-forge` or `backend: bf`
   - All checked workspaces declare `bead_cli.backend: bead-rs`

2. **bf binary not installed:**
   - `which bf` returns "not found" on both ex44 and lab
   - No bf executable in standard paths (`~/.local/bin/bf`, `/usr/local/bin/bf`)

3. **No bead-forge stores in active use:**
   - All live NEEDLE-workspace `.beads/` directories use bead-rs schema (`.beads/config.json` + checkpoint structure)
   - No legacy bead-forge `.beads/issues.jsonl` flat files remain in dispatched workspaces

4. **Migration beads complete:**
   - All beads tracking migration work under labels `bead-cli-backend` and `bf-removal` are closed
   - Phase 16 (pluggable backends rollout) marked complete in `docs/plan/plan.md`

## Consequences

**Positive:**
- **Code simplification:** ~200 lines of bf-specific logic deleted from `src/bead_store/mod.rs`, `src/prompt/mod.rs`, `src/worker/mod.rs`, `src/cli/mod.rs`, and `src/validation/predispatch.rs`
- **Test surface reduced:** No need to maintain bead-forge dialect fixtures or bf-specific integration tests
- **Zero ambiguity:** `auto` detection has exactly one resolution path — `bead` — eliminating the chimera hazard ADR-013 §5 identified
- **Operational clarity:** One backend to document, one CLI to install, one dialect to teach agents

**Neutral:**
- **Extensibility preserved:** The descriptor system remains; a future fourth backend could be added as user YAML without NEEDLE code changes
- **Binding invariant unchanged:** ADR-014's requirement that every workspace declare its backend in `.needle.yaml` still holds (now that value is always `bead-rs`)

**Risks mitigated:**
- **No rollback path from bead-rs to bead-forge:** This removal assumes bead-rs is sufficiently stable and complete that a workspace-level rollback is not required. If a critical defect in bead-rs makes fleet-wide rollback necessary, NEEDLE would need to restore `BfCliBeadStore` and the `bead-forge` descriptor from git history.
- **Bead-rs becomes a hard dependency:** NEEDLE cannot operate on any workspace without `bead` installed. This was already true in practice (all active workspaces use bead-rs), but it is now enforced by code rather than configuration.

### Superseded ADR-013 provisions

**ADR-013 §7 (backend priority table) is fully superseded:**
- ADR-013 stated: "`auto` detection prefers `bead`, then `bf`, then `br`" and assigned bead-forge as "secondary" priority
- This ADR removes the `bf` and `br` fallback paths entirely — `auto` now resolves only to `bead`
- The priority table no longer applies; there is only one backend

**ADR-013's rejection of "Standardize on one CLI and drop the others" is superseded:**
- ADR-013 rejected: "Standardize on one CLI and drop the others" because "all three upstreams are live and independently maintained"
- That rejection was correct for 2026-08-12 (bf was load-bearing)
- It is now incorrect for 2026-08-28 (bf is retired and uninstalled)
- This ADR accepts exactly what ADR-013 rejected, but on the basis that the premise (multiple live backends) no longer holds

### When to revisit

This decision should be revisited if:
1. **A critical bead-rs defect requires fleet-wide rollback to bead-forge** — this would require restoring `BfCliBeadStore` and the `bead-forge` descriptor from git history
2. **A new independent bead backend emerges as a production requirement** — this would restore multi-backend support, but with a different backend (not bead-forge)

## Evidence

**Verification performed 2026-08-28:**

```bash
# 1. bf binary not in PATH
$ which bf
bf not found in PATH

# 2. No bead-forge backend declarations in configuration
$ grep -r "backend:.*bead-forge\|backend:.*bf" /home/coding --include=".needle.yaml"
No bead-forge/bf backend declarations found

# 3. Sample .needle.yaml files all declare bead-rs
$ head -10 /home/coding/NEEDLE/.needle.yaml
bead_cli:
  backend: bead-rs

$ head -10 /home/coding/commitgraph/.needle.yaml
bead_cli:
  backend: bead-rs
```

**Code removal evidence:**
- Commit message for bead-forge descriptor removal (reference via `bf-removal` label)
- Test suite changes removing bf-specific fixtures and integration tests
- `src/bead_store/mod.rs` — `BfCliBeadStore` struct deleted (previously at `:1852`)
- `src/prompt/mod.rs` — bf-specific prompt templates deleted (previously at `:56`, `:64`, `:294-325`)

**Migration tracking:**
- All beads under label `bead-cli-backend` closed (Phase 16 backend descriptors and binding)
- All beads under label `bf-removal` closed (bf-specific code removal)
- Phase 16 marked complete in `docs/plan/plan.md`

**Historical context from ADR-013 (2026-08-12):**
- ADR-013 §7 priority table listed bead-forge as "secondary" backend with `bf` v0.4.1 installed at `~/.local/bin/bf`
- ADR-013 "Alternatives Considered" rejected standardization on one CLI because "all three upstreams are live and independently maintained"
- ADR-013 §5 identified a "chimera" hazard where `BrCliBeadStore` (beads_rust dialect) was bound to `bf` binary — resolved by ADR-014's explicit binding

**Historical context from ADR-014 (2026-08-12):**
- ADR-014 required explicit per-workspace `bead_cli.backend` binding in `.needle.yaml`
- ADR-014 §6 stated "The jedarden/bead-forge repository is a permanent explicit exception" — this exception is now moot as bead-forge itself is retired

---

**Rationale for superseding:** ADR-013's multi-backend design was correct for 2026-08-12, when bead-forge was load-bearing and fleet migration was just beginning. ADR-014 provided the migration mechanism. This ADR completes that migration by removing the now-unnecessary backend. The pattern — design migration infrastructure (ADR-013), deploy it (ADR-014), complete migration, then remove the legacy option — is the standard lifecycle for backend transitions.
