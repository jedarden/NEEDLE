# ADR-027: Policy Has Explicit Authority and Every Attempt Records Its Context

**Status:** Accepted — 2026-08-31

**Decision-makers:** NEEDLE maintainers and software-factory operators

## Context

Worker decisions are currently distributed across repository and global
AGENTS.md files, CLAUDE.md files, memory indexes and leaves, ADRs,
configuration, prompt templates, and executable gates. These surfaces can
contradict one another, and adapter-specific automatic loading depends on the
working directory. A result cannot be reproduced if the factory cannot say
which instructions and memory were actually presented to the worker.

The historical plan treated one workspace plan as the complete policy. That
does not reflect the deployed system and encourages silent policy drift.

## Decision

NEEDLE will define and enforce an explicit policy precedence and produce a
content-addressed ContextManifest for every attempt.

The default authority order is:

1. safety and external authority constraints;
2. applicable repository and nested AGENTS.md instructions;
3. applicable CLAUDE.md or adapter-specific instruction projections;
4. executable gates and workspace configuration within their declared scope;
5. accepted ADR decisions incorporated by the current plan;
6. the current plan and task acceptance criteria;
7. retrieved episodic memory and candidate lessons, which are advisory only.

Higher authority does not silently erase a conflict. A policy doctor reports
conflicting commands, backend declarations, lifecycle ownership, verification
rules, and stale superseded memory. Fatal ambiguity fails admission before a
claim; nonfatal ambiguity is attached to the attempt.

The ContextManifest records ordered source paths or stable identifiers,
content hashes, scopes, precedence decisions, prompt/template version,
configuration fingerprint, adapter and tool versions, retrieved-memory IDs,
and redaction metadata. NEEDLE explicitly injects the resolved bounded bundle
instead of relying on an adapter's current-directory loading behavior.

No learning component may directly modify authoritative policy. A promoted
proposal is applied by a separate, audited policy-promotion operation after
the required review and canary decision. Marker-managed projections may keep
AGENTS.md and CLAUDE.md aligned, but their source, version, and rollback must
remain explicit.

## Consequences

### Benefits

- Attempts become reproducible and policy conflicts become observable.
- Claude, Codex, and other adapters receive the same resolved task policy.
- Memory can augment policy without becoming a hidden authority.
- Changes to policy can be evaluated by version and rolled back.

### Costs and risks

- Policy parsing and projection must preserve repository-specific semantics.
- Some conflicts require human authority rather than automatic precedence.
- Manifests may reveal sensitive paths or metadata and therefore require
  sanitization before export.

## Implementation

1. Define the policy-source registry, scope rules, and manifest schema.
2. Add `needle doctor policy` with machine-readable diagnostics.
3. Reconcile the current NEEDLE AGENTS.md, CLAUDE.md, plan, memory, backend,
   checkpoint, and verification claims before enabling strict admission.
4. Record manifests in dispatch telemetry and trace storage.
5. Add adapter conformance tests proving explicit context equality.
6. Add an audited, reversible policy-promotion operation used only after a
   successful experiment and required approval.

## Related

- [Current software-factory plan](../plan/plan.md)
- [ADR-026: Evidence-gated reflection and derived memory](026-evidence-gated-reflection-and-derived-memory.md)
- [ADR-014: Explicit workspace bead backend binding](014-explicit-workspace-bead-backend-binding.md)
- [ADR-020: Verification gates judge committed state](020-verification-gates-judge-committed-state.md)

