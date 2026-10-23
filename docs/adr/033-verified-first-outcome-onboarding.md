# ADR-033: Onboarding ends with one isolated verified outcome

**Status:** Accepted — 2026-10-23; implementation pending
**Deciders:** operator request to implement the outcome-reliability roadmap
**Tracking:** plan revision 37, section 4.13; existing onboarding owners retained

## Context

Installation alone does not demonstrate a usable worker. Backend binding,
adapter availability, credentials, upstream configuration and verification can
each prevent the first result. Existing onboarding, checksum, init-preservation
and quickstart-smoke beads already own these defects. This decision gives them
one product acceptance boundary without creating a replacement installer.

## Decision

The supported first-use journey is install, inspect readiness, explicitly bind
an isolated workspace, rehearse one approved task, and inspect verified
completion. README, agent onboarding and executable quickstart use the same
maintained fixture and commands. Successful installation is reported separately
from successful execution and from verified delivery.

Readiness uses ADR-014 backend binding, capability negotiation and ADR-027
policy diagnostics. It checks the actual configured adapter/executable,
required authentication availability without exposing secrets, verification
configuration and any required Git upstream. A failing check explains the
remediation and which steps require an explicit command. Doctor remains
read-only unless an already-supported explicit repair mode is invoked. Init
preserves existing global and workspace configuration and refuses ambiguous
backend/store identity rather than initializing over it.

Offer an execution-only example profile: discovery, proposal generation,
learning mutation and self-modification are disabled in that profile. Existing
users' defaults and configuration are not silently changed. Release artifacts
and backend assets must satisfy the existing checksum contracts.

The automated rehearsal uses a temporary HOME, explicit discovery root and
state directory, a real configured bead backend, a deterministic adapter
fixture, and local disposable Git delivery targets. It verifies the intended
artifact and backend completion, not merely exit zero. Missing prerequisites
and failed verification produce actionable non-success. It must never touch
production stores, use paid credentials, send messages or push to public
remotes. A separately authorized real-provider smoke remains distinct evidence.

## Alternatives and consequences

A simulated successful worker without the real backend is insufficient.
Automatically installing or overwriting prerequisites during diagnosis is
rejected. Deterministic rehearsal cannot prove provider availability, so its
result explicitly states its adapter and environment.

## Acceptance and rollout

Existing owners implement their current boundaries: checksum validation,
non-destructive init, doctor remediation, shared quickstart documentation and
the isolated executable smoke. CI integration belongs to the existing NEEDLE
onboarding owner even when manifests live in declarative-config. Tests cover
fresh setup, missing backend/adapter, existing configuration, invalid checksum,
failed acceptance and required-but-missing upstream. Publish a supported path
only after the release smoke proves it. No new hosting or fleet activation is
authorized by this planning change.
