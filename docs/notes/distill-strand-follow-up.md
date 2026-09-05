# Distill strand follow-up

**Saved:** 2026-09-05

## Idea

Create a NEEDLE-native learning strand, tentatively **Distill** or **Reflect
V2**, and run a small subset of workers with that learning-selection profile.
Its purpose is to change the worker's prompt and context so it converts bounded
clusters of aggregated transcript and outcome evidence into candidate lessons
that can improve future runs.

This is not a plan-versus-artifact gap scan. Weave creates work for missing
artifacts. Distill mines verified history for reusable guidance that may reduce
rediscovery, failed attempts, duration, and token consumption.

## Required shape

- A deterministic materializer reads the append-only ARMOR ledger and creates
  stable, claimable, deduplicated evidence batches.
- The Distill strand selects one batch and returns a dispatch plan containing a
  learning prompt profile, bounded `ContextManifest`, and
  `candidate-lesson/v1` outcome contract.
- The ordinary worker state machine performs the agent dispatch. Do not invoke
  the learning model inside `Strand::evaluate()`.
- A valid result is a structured `CandidateLesson` or `NO_LESSON`.
- Candidates cite attempts, sessions, beads, commits, counterexamples, affected
  paths, expected effect, confidence, expiry, and proposed instruction target.
- Placement uses the narrowest applicable path scope. Split candidates whose
  common ancestor would be too broad.
- Deterministic constraints should generally become executable gates; temporary
  findings remain episodic memory instead of bloating instruction files.
- Distill never directly edits `AGENTS.md`, `CLAUDE.md`, gates, ADRs, skills, or
  source. A separate reviewed, marker-fenced, receipted promotion operation
  performs those changes and supports rollback.

## NEEDLE change required

Today the selected strand name is retained for telemetry, but ordinary prompt
construction still selects the Pluck template unless split mode is active.
Make strand selection return a first-class prompt profile, context manifest,
and outcome contract. Avoid an implicit `if strand_name == "distill"` mapping.

## Pilot

Start report-only on one repository with a bounded historical evidence set.
Measure evidence precision, useful-candidate acceptance, instruction scope,
secret leakage, first-attempt verification, retries, duration, token use, and
regressions. Promote nothing automatically during the pilot.

Full rationale: `docs/research/distill-strand-aggregated-transcript-learning.md`.
