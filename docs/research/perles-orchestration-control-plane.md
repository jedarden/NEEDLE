# Perles: BQL Query Language and Multi-Agent Control Plane

## Research Date: 2026-03-20
## Source: https://github.com/zjrosen/perles

## What It Is

Perles is a Go-based terminal UI for the beads issue tracker (original `bd` lineage) that goes beyond visualization into orchestration. It features BQL (Beads Query Language) for structured queries, a multi-agent control plane with Coordinator/Worker architecture, and a community workflow system. It targets the original beads (Dolt backend), not beads_rust.

## BQL (Beads Query Language)

A purpose-built query language for filtering issues with:
- Boolean operators
- Date range filters
- Sorting
- Relationship expansion (traversing dependency graphs)

This enables agents and orchestrators to express complex queries against the bead graph -- something neither `bd ready` nor `br ready` support natively. Example use cases:
- "All P0 bugs blocked by beads assigned to agent-alpha"
- "Beads in the auth epic that have been in_progress for > 2 hours"
- "Ready beads excluding those labeled needs-decision"

## Multi-Agent Control Plane

### Architecture

Two-tier model:
- **Coordinator**: A single headless agent that receives workflow instructions and manages the session. Responsible for task decomposition, worker assignment, and progress tracking.
- **Workers**: Multiple headless agents that execute specific sub-tasks. Each worker specializes in a role: coding, testing, reviewing, or documenting.

### Worker Phase Lifecycle

Workers in "Cook" workflows cycle through defined states:
```
impl -> review -> await -> feedback -> commit
```

This is more granular than NEEDLE's outcome model (success/failure/timeout/crash). Perles tracks the *phase within execution*, not just the outcome.

### Workflow Templates

The `communityworkflows/` directory contains pre-built workflow definitions. Workflows operate on beads epics -- hierarchical task structures. The lifecycle:
1. Research workflows generate proposals
2. Proposals decompose into epic structures with individual tasks
3. Implementation workflows execute against those task hierarchies

## Key Design Decisions

1. **Query language over queue**: While NEEDLE treats beads as a FIFO queue (sorted by priority), Perles treats them as a queryable database. This enables more sophisticated work selection -- agents can query for specific types of work rather than just "next highest priority."

2. **Coordinator/Worker split**: Unlike NEEDLE where all workers are identical and compete for the same queue, Perles has a dedicated Coordinator that makes assignment decisions. This eliminates claim races but introduces a single point of failure.

3. **Phase-aware execution**: Workers report their current phase (impl, review, etc.), enabling the Coordinator to make scheduling decisions based on workflow progress, not just bead status.

4. **Community workflows**: Pre-built templates for common patterns. This is a marketplace approach to orchestration that NEEDLE's YAML adapters could learn from.

## Relevance to NEEDLE

### Advantages of Perles' Approach

1. **Richer work selection**: BQL could express "give me beads that match these criteria" rather than NEEDLE's "give me the highest-priority unblocked bead." If NEEDLE ever needs more sophisticated selection (e.g., routing GPU-heavy work to specific workers), a query language would help.

2. **No claim races**: The Coordinator assigns work directly. No SQLite contention, no thundering herd, no retry loops. The price is a single point of failure.

3. **Phase awareness**: Knowing that a worker is in "review" phase vs. "impl" phase enables better scheduling. NEEDLE treats execution as opaque -- it dispatches and waits for an outcome.

### Advantages of NEEDLE's Approach

1. **No single point of failure**: NEEDLE workers are independent. If one dies, others continue. Perles' Coordinator is a bottleneck.

2. **Agent-agnostic**: NEEDLE works with any headless CLI. Perles is more tightly coupled to its Coordinator/Worker model.

3. **Explicit outcome handling**: NEEDLE has defined handlers for every outcome. Perles' documentation is less explicit about failure modes.

4. **br-native**: NEEDLE works with beads_rust. Perles requires the original beads with Dolt.

### What NEEDLE Could Adopt

- **Query-based selection**: Instead of `br ready --json | sort`, NEEDLE could support configurable selection criteria.
- **Community workflow templates**: NEEDLE's YAML adapters could be published as shareable templates.
- **Phase reporting**: Workers could report their current phase (building prompt, executing agent, evaluating outcome) for better observability.
