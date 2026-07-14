# ADR: Use Read

**Decision ID:** nd-3c9f
**Date:** 2026-07-09

## Context

Analysis of options during task execution

## Alternatives Considered

1. Considered alternatives

## Decision

Use Read

## Rationale

Hmm, both commands just dump the config instead of setting the value. Looking at the output, `worker.max_workers` is still 20, not 10. This suggests the `--set` flag isn't actually setting the value. 

## Outcome

Decision implemented (success)
