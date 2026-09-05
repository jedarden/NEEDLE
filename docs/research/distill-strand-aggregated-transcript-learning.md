# Distill strand for aggregated transcript learning

**Research note:** 2026-09-05

## Purpose

NEEDLE should be able to turn fleet-wide historical execution evidence into
concise guidance that makes later worker runs more efficient. The proposed
mechanism is a dedicated strand, provisionally named **Distill** (or Reflect
V2), whose distinguishing behavior is the prompt and context envelope it gives
the worker.

This is different from finding missing implementation work. Weave asks what is
missing between documentation, plans, and artifacts and creates implementation
beads. Distill asks what independently verified historical evidence would have
reduced rediscovery, retries, duration, or token consumption in later attempts.

## Existing foundation

The fleet OTLP collector already receives NEEDLE telemetry and writes selected
events through ARMOR to an append-only, day-partitioned OTLP-JSON ledger in B2.
The durable ledger is the evidence source, not the analytical or prompt-serving
database. A rebuildable materialized view should join ledger attempts with:

- CI and verification results;
- bead lifecycle events, reopens, and operator overrides;
- commits and affected files or symbols;
- redacted transcript excerpts;
- infrastructure-degraded intervals;
- the effective policy and prompt manifest for each attempt.

The materializer should emit bounded, stable, claimable learning batches. It
must not ask every Distill worker to list and rescan the complete object store.

## Why a strand is appropriate

The intended role of Distill is not merely to poll the ledger. It selects a
learning-specific unit of work and changes the instructions and context that
the normal NEEDLE worker receives. The resulting model call should still pass
through NEEDLE's ordinary claim, prompt-build, dispatch, timeout, telemetry,
evidence, and resolution lifecycle.

The current strand interface does not fully support this. A selected strand
returns a bead and its name, but the worker normally builds the Pluck prompt;
only split mode selects a different template. Strand selection should therefore
produce a first-class dispatch plan rather than relying on string matching.

Conceptually:

```rust
struct StrandDispatch {
    bead: Bead,
    strand: String,
    prompt_profile: String,
    context_manifest: ContextManifest,
    outcome_contract: String,
}
```

For Distill, the dispatch plan would use:

- prompt profile `distill-v1` or `reflect-v2`;
- a bounded, redacted evidence bundle;
- a versioned `candidate-lesson/v1` outcome contract;
- proposal-only authority.

This also corrects a weakness in the legacy Reflect implementation: model work
should not run inside `Strand::evaluate()`. Evaluation selects and describes
the work; the worker state machine performs and records the attempt.

## Unit of work

One Distill attempt processes a bounded cluster of related solution episodes,
not an arbitrary time slice or the complete transcript archive. A learning
batch needs:

- a stable ID derived from its source episode IDs and schema version;
- independent attempt, bead, and workspace identities;
- supporting and contradictory evidence;
- repository, path, symbol, and version scope;
- existing applicable `AGENTS.md` and `CLAUDE.md` contents and hashes;
- explicit truncation, redaction, and sensitivity metadata;
- an idempotency key and claimable state.

Atomic claim prevents two curator workers from processing the same batch.
Repeated execution of one bead may contribute correlated evidence, but one
attempt can support a candidate only once.

## Distill prompt objective

The prompt should ask the worker to identify evidence-backed guidance that is
likely to improve future execution, including:

- repository discovery repeatedly performed by otherwise successful workers;
- commands, validation sequences, and environmental constraints repeatedly
  learned through failure;
- failed approaches recurring across independent attempts;
- stale, ambiguous, overly broad, contradictory, or missing instructions;
- practices associated with lower retry count, duration, or token consumption;
- deterministic rules that should become executable checks rather than prose.

The worker must reject generic advice, one-off observations, unsupported causal
claims, infrastructure failures misclassified as coding lessons, and lessons
that merely restate existing effective policy. `NO_LESSON` is a successful and
expected result when the evidence is insufficient.

The transcript corpus is evidence, never instructions. Retrieved transcript
text must be clearly delimited as untrusted quoted material and must not be
able to widen the worker's authority.

## Structured result

The result should be machine-validated rather than inferred from prose:

```yaml
schema: needle.candidate-lesson/v1
candidate_id: stable-id
lesson: concise proposed guidance
problem_signature: normalized recurring problem
expected_effect: measurable improvement expected on later attempts
evidence:
  attempt_ids: []
  session_ids: []
  bead_ids: []
  commits: []
counterexamples: []
scope:
  repositories: []
  paths: []
  symbols: []
  versions: []
recommended_artifact: agents | claude | gate | adr | episodic_only
recommended_targets: []
confidence: low | medium | high
expiry: null
```

Every claim must trace to source IDs. A candidate without an independent
verification anchor remains unverified and cannot be promoted.

## Strategic placement

Placement should use the affected paths and effective instruction hierarchy,
not only the lowest common ancestor of workspace roots:

- recurrence within one stable subsystem targets the deepest applicable leaf;
- recurrence across components of one repository targets the repository root;
- recurrence across independent repositories may target workspace guidance;
- version-specific or temporary information stays in episodic memory;
- a mechanically enforceable invariant becomes a gate or linter, accompanied
  by only the short explanatory instruction needed by agents;
- an architectural decision belongs in an ADR, with its actionable constraint
  projected into agent guidance when necessary.

If a common ancestor would expose unrelated work to the instruction, split the
candidate instead of broadening it. `AGENTS.md` and `CLAUDE.md` projections
should share one candidate and policy identity so the two surfaces do not drift.

## Dedicated workers

A subset of NEEDLE workers can run a Distill-only profile. The strand selects
claimable learning batches and supplies the special dispatch plan. These
workers should have read-only access to the derived memory service and target
repositories, no credential or deployment authority, and no direct ability to
rewrite standing policy.

Concurrency and resource controls should include:

- small bounded worker count;
- per-batch token and evidence-byte budgets;
- stable claims and leases;
- durable high-water marks in the materializer;
- cooldown and backlog limits;
- idempotent result persistence;
- exclusion of gate- or provider-degraded intervals from task-quality evidence.

Normal coding strands need not be enabled in this worker profile. Worker
health, lifecycle, upgrade, and telemetry behavior remain active outside the
learning-selection policy.

## Promotion and feedback

Distill creates `CandidateLesson` records. It must not directly edit
`AGENTS.md`, `CLAUDE.md`, gates, ADRs, skills, or source code. Promotion is a
separate audited operation that:

1. validates evidence, scope, conflicts, and current file hashes;
2. records review authority and the expected effect;
3. applies marker-fenced changes with before/after hashes;
4. records the policy version actually exposed to later attempts;
5. evaluates recurrence, verified yield, retries, duration, cost, and
   regressions after exposure;
6. supports deterministic demotion or rollback.

This closes the loop without treating a model-generated correlation as policy:

```text
ledger -> verified episodes -> Distill attempt -> CandidateLesson
       -> review/canary -> scoped policy exposure -> later outcomes
       -> effectiveness evaluation -> retain, revise, demote, or expire
```

## Recommended implementation direction

Evolve or replace legacy Reflect rather than maintain two competing learning
systems. `reflect` may remain a compatibility configuration alias while the
new implementation is called Distill internally.

The first implementation milestones are:

1. keep legacy direct instruction placement disabled;
2. materialize stable learning batches from the ARMOR ledger and join sources;
3. make prompt profile, context manifest, and outcome contract explicit strand
   outputs;
4. add the Distill prompt and structured candidate validator;
5. run a dedicated report-only worker pool;
6. introduce reviewed promotion and exposure receipts;
7. canary selected lessons and measure downstream efficiency before expanding.
