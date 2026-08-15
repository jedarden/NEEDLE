# Bead backend fleet migration audit (2026-08-15)

This note records the completion evidence for the Jed Arden bead workspace
migration. Forgejo is the source of truth; GitHub is a read-only mirror and was
used only to discover possible historical repositories.

## Result

- The local `/home/coding` fleet contains 65 bead workspaces. NEEDLE's
  `bead-backend-audit` reports all 65 explicitly bound to `bead-rs`, with zero
  unbound workspaces.
- Direct Forgejo probes found five source-of-truth repositories among the
  remaining public legacy signatures. Four are now native and explicitly bound
  to `bead-rs`: `SIGIL` (`54d5d383`), `domain-check` (`61d27ac`), `miroir`
  (`323aaad5`), and `agentists-quickstart-deprecated` (`40f85602`).
- `bead-forge` is the deliberate permanent exception. Commit `c9d094e0`
  explicitly binds it to the `bead-forge` backend.
- `bead-rs` itself was already converted by `09aa945`; its empty native store,
  checkpoint, explicit binding, and Forgejo synchronization were reverified.

The public GitHub mirrors for `miroir` and
`agentists-quickstart-deprecated` still showed older legacy content during the
audit, but their Forgejo `main` branches contain the native cutovers above.

## Rehydrated stores

`SIGIL` was rehydrated from 739 source issues and 666 dependency edges. Its 23
item ready frontier, issue fields, labels, graph, and clean-clone checkpoint
restore were reconciled exactly.

`domain-check` was rehydrated from 184 source issues and 157 dependency edges.
Three open issues carried stale `glm-bravo` assignments while bead-forge still
reported them ready; those assignments alone were cleared so bead-rs preserved
the 12-item source ready frontier. All other fields, labels, graph structure,
and the clean-clone restore reconciled exactly.

Rollback copies and machine-readable reconciliation reports remain under
`/home/coding/scratch`:

- `SIGIL-beadforge-rollback-20260815`, `SIGIL-beadforge-final-20260815`, and
  `SIGIL-reconciliation-20260815.json`
- `domain-check-beadforge-rollback-20260815`,
  `domain-check-beadforge-final-20260815`, and
  `domain-check-reconciliation-20260815.json`

Dedicated gitleaks scans of both migration artifact sets found no secrets.

## Ineligible historical mirrors

Nine public GitHub repositories retained legacy bead signatures but had no
corresponding Forgejo repository when probed over Git transport: `AMAIL`,
`NEEDLE-deprecated`, `agent-definitions`, `aravalli`, `beads`, `beads_viewer`,
`lending-prototype`, `native-ads-profiler`, and `ringmaster`. They are
historical/orphaned mirrors, not eligible writable repositories under the
Forgejo source-of-truth policy, so this migration did not mutate them.

Forgejo's REST repository-list endpoint returned HTTP 403 with the available
credential. The audit therefore combined the complete local fleet scan, the
public GitHub repository inventory, and direct `git ls-remote` probes against
Forgejo for each legacy candidate.

## Verification

The clean NEEDLE checkout passed the descriptor and CLI-store tests (20 tests),
the ignored real mixed-backend isolation gate, the ignored real bead-rs
lifecycle test, both targeted real bead-forge claim/recovery tests, and the
canary library tests (40 tests). `cargo fmt --check` remains blocked by
pre-existing formatting drift outside this migration's changed code.

The authoritative `needle-ci` workflow run `needle-ci-6gxtz` could not reach
source verification because its clone step lacked Forgejo credentials
(`fatal: could not read Username`). This was an infrastructure authentication
failure, not a test failure, and no ArgoCD-managed resource was mutated as a
workaround.
