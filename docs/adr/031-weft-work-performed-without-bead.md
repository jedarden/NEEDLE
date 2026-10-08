# ADR-031: Weft completes LOOM turns without manufacturing a bead

**Status:** Accepted
**Date:** 2026-10-08
**Tracking:** needle-0ea049d0

## Context

LOOM's worker queue hands NEEDLE turns that can be answered directly, such as a question or a report. Those are completed work even though no NEEDLE bead is created. Treating them as NoWork would let the waterfall classify a completed turn as idle or starved.

The worker API also has lease and retry semantics that must remain consistent: transient transport and server failures get bounded retries; request 4xx errors are terminal; claim conflicts are queue contention except when LOOM confirms that the same worker's earlier claim committed; and a lost lease ends local processing.

## Decision

Weft uses a dedicated ureq client for LOOM's worker endpoints. It sends bearer authentication, the configured worker identity, and JSON accept headers. Token files are read only when a request is made; a 401 disables requests while the same token remains configured and logs weft.token_rejected without exposing the token.

StrandResult::WorkPerformed { summary, telemetry } records a completed external turn without a bead. The waterfall emits a work_performed strand result and cycle outcome, then returns to selection without entering the idle or starvation path. Weft remains a selector (is_generator() == false).

Adapter execution is supplied through WeftTurnExecutor; the protocol client does not encode a particular harness. An unsupported contract_version is failed server-side as contract_unsupported, retryable: false. A 409 lease_lost stops processing and drops the local result.

## Consequences

- Completed answers and reports are visible as useful work in cycle telemetry.
- A repeated same-worker claim response is idempotent; another worker's claim is ordinary contention.
- The adapter layer can evolve independently while sharing one wire client and one result contract.
- After transient failures exhaust the 1s/5s/30s schedule, LOOM lease expiry recovers the turn.
