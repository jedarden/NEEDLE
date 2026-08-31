# ADR-026: Reflection Produces Evidence-Gated Lessons and Derived Memory

**Status:** Accepted — 2026-08-31

**Decision-makers:** NEEDLE maintainers and software-factory operators

## Context

The current Reflect implementation extracts observations from transcripts and
closed-bead scans, reinforces similar text by count, writes workspace learning
files, and contains paths for cross-workspace and CLAUDE.md promotion. In
practice, generic tool outcomes and repeated redispatches can dominate the
learning corpus. Reinforcement count is not independent evidence, and editing
an instruction file makes a hypothesis into policy before its effect is known.

The factory needs to improve from experience without optimizing for visible
process, storing private model reasoning, or turning transient correlations
into fleet-wide rules.

## Decision

Reflection will be exception-driven and evidence-gated. It will consume
versioned attempts, resolutions, verifier results, incidents, and external
effect receipts. It will not ingest or retain hidden chain-of-thought. A
concise rationale deliberately written for the record is permitted.

The promotion lifecycle is:

```text
observation -> candidate lesson -> validated runbook
            -> reviewed policy proposal -> canary -> promoted or expired
```

A reflection episode records a problem, failure fingerprint, hypothesis,
intervention, evidence, counterexample, scope, confidence, and expiry. A
candidate lesson has a stable ID; an attempt can support it at most once.
Repeated text, repeated execution of one bead, or one worker's confidence does
not count as independent reinforcement.

Reflection is triggered by bounded, meaningful events: false closure or
reopen, repeated causal fingerprint, quarantine, verification bypass,
rollback, regression, policy conflict, or successful recovery after a prior
failure. A small scheduled sample of ordinary success may be used to detect
blind spots; a retrospective is not required for every closure.

NEEDLE will maintain four distinct planes:

1. **Work truth:** bead-rs state plus attempts, evidence, and resolutions.
2. **Policy:** AGENTS.md, CLAUDE.md, accepted ADRs, configuration, and
   executable gates.
3. **Episodic memory:** incidents, runbooks, decisions, and reviewed memory
   leaves.
4. **Derived memory index:** a rebuildable scoped retrieval catalog whose
   entries point to source artifacts and hashes.

The derived index is never authoritative. Embeddings and rankings are caches.
Reflect writes proposals to a review queue, not directly to AGENTS.md,
CLAUDE.md, gates, or executable code. Promotion to policy requires evaluation,
canary guardrails, and the authority specified by ADR-027.

An approved experiment may automatically change derived-memory ranking,
select among already approved prompt or policy variants, tune controller
cadence and retry budgets within declared numeric bounds, and advance a
candidate into or out of a canary. These changes are versioned exposures, not
new authority. Creating a new rule, editing source or instruction files,
weakening a gate, or widening a controller's permissions remains outside the
automatic envelope.

## Consequences

### Benefits

- Lessons are tied to independently identifiable evidence.
- The factory can measure whether a lesson reduces recurrence or retry cost.
- Memory retrieval becomes consistent across working directories and agent
  adapters.
- Stale or harmful lessons can expire or be demoted.
- Policy remains reviewable and reversible.

### Costs and risks

- Existing `.beads/learnings.md` content becomes legacy input and cannot be
  bulk-promoted.
- A memory catalog, proposal queue, evaluators, and retention policy must be
  built.
- Evidence gates slow promotion deliberately; high-confidence emergency
  mitigations still need an explicit reversible fast path.

## Implementation

1. Disable direct instruction-file promotion and count-only reinforcement.
2. Define versioned ReflectionEpisode, CandidateLesson, PolicyProposal,
   PolicyExperiment, and LessonEffectiveness records.
3. Import existing learnings only as untrusted candidates with source hashes.
4. Build a scoped MemoryCatalog with validity, supersession, sensitivity, and
   evidence metadata.
5. Retrieve a bounded context bundle and record it in each ContextManifest.
6. Evaluate candidates against verified-closure yield, recurrence,
   retry-amplification, cost, and regression guardrails.
7. Promote, demote, or expire through explicit reviewed transitions.

## Related

- [Current software-factory plan](../plan/plan.md)
- [ADR-024: Attempt, evidence, and resolution](024-attempt-evidence-resolution-is-the-unit-of-work.md)
- [ADR-025: Independent reconciling controllers](025-independent-reconciling-controllers-over-idle-waterfall.md)
- [ADR-027: Policy authority and context manifests](027-policy-authority-and-context-manifests.md)
