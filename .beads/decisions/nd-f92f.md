# ADR: Use Bash

**Decision ID:** nd-f92f
**Date:** 2026-07-16

## Context

Analysis of options during task execution

## Alternatives Considered

1. Considered alternatives

## Decision

Use Bash

## Rationale

Ah, I see! The syntax has changed. Now instead of:
```
br dep add <blocked_id> <blocker_id> --type blocks
```

It should be:
```
br dep add <blocker_id> --blocks <blocked_id> --type blocks
```

Or mor

## Outcome

Decision implemented (success)
