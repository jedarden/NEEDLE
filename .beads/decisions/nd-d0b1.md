# ADR: Respond to user

**Decision ID:** nd-d0b1
**Date:** 2026-08-07

## Context

Analysis of options during task execution

## Alternatives Considered

1. Considered alternatives

## Decision

Respond to user

## Rationale

Now let me look at the exact issue and create a proper fix. The problem is in the `cmd_stats` function which hardcodes the log directory instead of using the configured one like `cmd_logs` does.

## Outcome

Decision implemented (success)
