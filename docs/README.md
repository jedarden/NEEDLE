# NEEDLE Documentation Index

This directory contains 148 markdown files documenting NEEDLE's architecture, design decisions, operations, and research. This index is the entry point for finding what you need.

## Quick Navigation

- **[Start Here](#start-here)** — New to NEEDLE? Begin here
- **[Architecture](#architecture)** — System design and components
- **[ADRs](#architecture-decision-records-adrs)** — Design decisions and rationale
- **[Operations](#operations)** — Running and maintaining NEEDLE fleets
- **[Investigations & Post-Mortems](#investigations--post-mortems)** — Incident analyses
- **[Reference](#reference)** — Schemas, verification reports, technical specs

---

## Start Here

New to NEEDLE? Start with these guides to get up and running quickly.

| Document | Description |
|----------|-------------|
| **[Quickstart Example](examples/quickstart/README.md)** | End-to-end walkthrough from empty workspace to first closed bead |
| **[Quickstart Expected Output](examples/quickstart/expected-output.md)** | Reference output for the quickstart example |
| **[Configuration Guide](configuration.md)** | Complete configuration reference (config.yaml, environment variables, CLI args) |
| **[Agent Adapter Authoring](plan/plan.md#agent-adapters)** | How to write custom agent adapters (invoke_template schema) |
| **[Plugin: Claude Interactive](templates/AGENTS-needle.md)** | NEEDLE workspace template for Claude Code sessions |

---

## Architecture

NEEDLE's design principles, component architecture, and implementation phases.

### Core Architecture

| Document | Description |
|----------|-------------|
| **[Implementation Plan](plan/plan.md)** | Master plan: principles, components, phases 1-4 |
| **[Bead-forge to Bead-rs Rehydration Playbook](plan/bead-forge-to-bead-rs-rehydration-playbook.md)** | Migration playbook from bead-forge to bead-rs |
| **[Agent Event Schema](agent-event-schema.md)** | Structured events emitted during agent execution |
| **[Capabilities Negotiation](capabilities-negotiation.md)** | Backend capability verification contract |

### Design Documents

| Document | Description |
|----------|-------------|
| **[Definition of Done (Design)](design/definition-of-done.md)** | Design specification for DoD system |
| **[DoD Activation Status](design/definition-of-done-activation-status.md)** | Implementation status of DoD components |
| **[Timeout Decomposition Types](design/timeout-decomposition-types.md)** | Type-level design for timeout handling |
| **[Unified DoD Status](design/unified-definition-of-done-status.md)** | Unified status representation for definition-of-done |
| **[Workspace Iteration Fix](design/workspace-iteration-fix.md)** | Design fix for workspace iteration behavior |

---

## Architecture Decision Records (ADRs)

NEEDLE's major design decisions, recorded as ADRs with status tracking.

| Number | Title | Status |
|--------|-------|--------|
| [ADR-001](adr/001-explore-strand-hardening.md) | Explore Strand Hardening | Accepted |
| [ADR-002](adr/002-pluck-telemetry-isolation-and-process-tracking.md) | Pluck Telemetry Isolation and Process Tracking | Accepted |
| [ADR-003](adr/003-cleanup-orphan-detection-gap.md) | Cleanup Command Orphan-Detection Gap | Accepted |
| [ADR-004](adr/004-recursive-workspace-discovery-default.md) | Recursive Workspace Discovery as Default | Accepted |
| [ADR-005](adr/005-unify-release-upgrade-with-canary-hot-reload.md) | Unify GitHub-Release Upgrade with Canary Hot-Reload | Accepted |
| [ADR-006](adr/006-bead-lifecycle-reliability.md) | Bead Lifecycle Reliability | Accepted |
| [ADR-007](adr/007-deploy-path-hardening.md) | Deploy-Path Hardening | Accepted |
| [ADR-008](adr/008-fleet-resource-safety.md) | Fleet Resource Safety | Planned |
| [ADR-009](adr/009-external-adopter-hardening.md) | External-Adopter Hardening | Implemented |
| [ADR-010](adr/010-supervisor-zombie-reaping.md) | Supervisor Zombie-Child Reaping | Planned |
| [ADR-011](adr/011-mitosis-timeout-child-reaping.md) | Mitosis Timeout Child Reaping | Proposed |
| [ADR-012](adr/012-quarantine-wiring-and-failure-aware-pluck-ordering.md) | Quarantine Wiring and Failure-Aware Pluck Ordering | Implemented |
| [ADR-013](adr/013-pluggable-bead-cli-backends.md) | Pluggable Bead-CLI Backends | Accepted |
| [ADR-014](adr/014-explicit-workspace-bead-backend-binding.md) | Explicit Workspace Bead Backend Binding | Accepted |
| [ADR-015](adr/015-concurrent-same-repo-worker-isolation.md) | Concurrent Same-Repo Worker Isolation | Accepted |
| [ADR-016](adr/016-otlp-resource-propagation-and-roaming-worker-identity.md) | OTLP Resource Propagation and Roaming-Worker Identity | Proposed |
| [ADR-017](adr/017-configuration-hot-reload-at-the-cycle-boundary.md) | Configuration Hot-Reload at Cycle Boundary | Proposed |
| [ADR-018](adr/018-reopen-assignee-contract.md) | Bead Reopen Assignee Contract | Accepted |
| [ADR-019](adr/019-explore-strand-activation-conditions.md) | Explore Strand Activation Conditions | Accepted |
| [ADR-020](adr/020-verification-gates-judge-committed-state.md) | Verification Gates Judge Committed State | Accepted |
| [ADR-021](adr/021-bead-forge-removal.md) | Removal of bead-forge (bf) Backend Support | Accepted |

---

## Operations

Running, maintaining, and operating NEEDLE fleets.

### System Operations

| Document | Description |
|----------|-------------|
| **[Binary Freshness Verification](binary-freshness-verification.md)** | Verifying automatic worker rotation on binary updates |
| **[Binary Freshness Status](binary-freshness-verification-status.md)** | Status tracking for binary freshness feature |
| **[Heartbeat System](heartbeat.md)** | Worker heartbeat protocol and peer monitoring |
| **[Definition of Done](definition-of-done.md)** | DoD adoption guide and pattern reference |
| **[DoD Pattern](definition-of-done-pattern.md)** | Reusable DoD pattern for bead deliverables |
| **[DoD Adoption Guide](definition-of-done-adoption-guide.md)** | How to adopt DoD in your workspace |
| **[Verification Runner Adoption Guide](adoption-guide.md)** | Adopt the configurable YAML verification runner |
| **[Marathon Instruction](marathon-instruction.md)** | Long-running session patterns |

### Checkpoint & Recovery

| Document | Description |
|----------|-------------|
| **[Checkpoint Tracking](checkpoint-tracking.md)** | How checkpoint files track bead state |
| **[Checkpoint Cleanup Strategy](checkpoint-cleanup-strategy.md)** | Cleanup policies for old checkpoints |
| **[Checkpoint Commit Workflow](checkpoint-commit-workflow.md)** | Git integration for checkpoint commits |
| **[Checkpoint Publishing](checkpoint-publishing.md)** | Publishing checkpoints to remote stores |

### Health & Diagnostics

| Document | Description |
|----------|-------------|
| **[Mend Audit Report](mend_audit_report.md)** | Mend strand audit results |
| **[Claim Span Audit](claim-span-audit.md)** | Claim operation latency audit |
| **[Full Latency Profile](full-latency-profile.md)** | End-to-end latency analysis |

### Explore Strand

| Document | Description |
|----------|-------------|
| **[Explore Configuration](explore-config.md)** | Explore strand configuration options |
| **[Explore Access Patterns](explore-access-patterns.md)** | How Explore accesses workspaces |
| **[Explore Access Paths](explore-access-paths.md)** | Access path analysis |
| **[Explore Access Map](explore-strand-access-map.md)** | Workspace access decision map |
| **[Explore Access Decision Tree](explore-strand-access-decision-tree.md)** | Decision tree for Explore access |
| **[Explore Strand Access Patterns](explore-strand-access-patterns.md)** | Strand-level access patterns |
| **[Explore Workspace Discovery](explore-workspace-discovery.md)** | Workspace discovery mechanisms |
| **[Explore Strand Workspace Discovery](explore-strand-workspace-discovery.md)** | Strand-specific discovery |
| **[Explore Audit Report](explore-audit-report.md)** | Explore strand audit results |
| **[Explore Config Init Flow](explore-config-init-flow.md)** | Configuration initialization flow |
| **[Explore Test Classification](explore-test-classification.md)** | Test classification for Explore |

---

## Investigations & Post-Mortems

Incident analyses, debugging sessions, and operational investigations (newest first).

### 2026-08 Incidents

| Document | Date | Description |
|----------|------|-------------|
| **[Clippy Findings 2026-08-29](clippy-findings-2026-08-29.md)** | 2026-08-29 | Clippy lint results and fixes |
| **[Clippy Findings 2026-08-29](clippy-findings-2026-08-29.md)** | 2026-08-29 | Clippy lint results and fixes |
| **[Issue 16 Comment Loop](notes/2026-08-29-issue-16-comment-loop.md)** | 2026-08-29 | GitHub issue #16 comment loop analysis |
| **[Test Isolation Consolidated Findings 2026-08-28](test-isolation-consolidated-findings-2026-08-28.md)** | 2026-08-28 | Test isolation consolidated findings |
| **[Test Isolation Audit 2026-08-28](test-isolation-audit-2026-08-28.md)** | 2026-08-28 | Test isolation audit report |
| **[Anthropic Routing Verification 2026-08-28](notes/anthropic-routing-verification-2026-08-28.md)** | 2026-08-28 | Anthropic model routing verification |
| **[GLM-4.7 Routing Verification 2026-08-28](notes/glm-4.7-routing-verification-2026-08-28.md)** | 2026-08-28 | GLM-4.7 routing verification |
| **[False Close Audit 2026-08](notes/false-close-audit-2026-08.md)** | 2026-08 | False close analysis |
| **[Test Isolation Audit 2026-08-28](test-isolation-consolidated-findings-2026-08-28.md)** | 2026-08-28 | Test isolation consolidated findings |
| **[Test Isolation Audit 2026-08-28](test-isolation-audit-2026-08-28.md)** | 2026-08-28 | Test isolation audit report |
| **[Production OTLP Configuration 2026-08-15](production-otlp-configuration-2026-08-15.md)** | 2026-08-15 | OTLP configuration for production |
| **[Needle CI Failure 2026-08-16](needle-ci-failure-investigation-2026-08-16.md)** | 2026-08-16 | CI failure investigation |
| **[Production OTLP Configuration 2026-08-15](production-otlp-configuration-2026-08-15.md)** | 2026-08-15 | OTLP configuration for production |
| **[Pulse Strand Enablement 2026-08-15](notes/pulse-strand-enablement-2026-08-15.md)** | 2026-08-15 | Pulse strand enablement analysis |
| **[Bead-rs Fleet Migration 2026-08-15](notes/bead-rs-fleet-migration-2026-08-15.md)** | 2026-08-15 | Bead-rs migration notes |
| **[Needle CI Failure 2026-08-16](needle-ci-failure-investigation-2026-08-16.md)** | 2026-08-16 | CI failure investigation |

### Historical Investigations (Pre-2026-08)

| Document | Description |
|----------|-------------|
| **[GitHub Draft Releases Root Cause](github-draft-releases-root-cause-analysis.md)** | GitHub release drafting failure analysis |
| **[GH Release Draft Root Cause](gh-release-draft-root-cause.md)** | Release drafting incident |
| **[Post-Push CI](post-push-ci.md)** | Post-push CI behavior |
| **[Sanitize Latency CI](sanitize_latency_ci.md)** | Sanitization latency in CI |
| **[Child Wait Analysis](child_wait_analysis.md)** | Child process waiting behavior |
| **[Compilation Error Detection](compilation-error-detection.md)** | Compilation error detection patterns |
| **[Concurrent Claim Cycle Safety](concurrent-claim-cycle-safety-validation.md)** | Claim cycle safety validation |
| **[Coverage Gap](coverage-gap.md)** | Test coverage analysis |
| **[P95 Calculation Algorithms](p95-calculation-algorithms.md)** | Percentile calculation methods |
| **[Process Spawning Test Catalog](process-spawning-test-catalog.md)** | Process spawning test inventory |
| **[Process Spawning Test Summary](process-spawning-test-summary.md)** | Process spawning tests summary |
| **[Test Output Handling](test-output-handling-analysis.md)** | Test output processing |
| **[Test Output](test_output.md)** | Test output analysis |
| **[Test Stack Trace Capture](test-stack-trace-capture.md)** | Stack trace capture in tests |
| **[Tilde Expansion Config Fields](tilde-expansion-config-fields.md)** | Tilde expansion in configuration |
| **[P95 Calculation Algorithms](p95-calculation-algorithms.md)** | Percentile calculation methods |
| **[Tilde Expansion Config Fields](tilde-expansion-config-fields.md)** | Tilde expansion in configuration |
| **[Timeout Mitosis Decomposition Design](timeout-mitosis-decomposition-design.md)** | Mitosis timeout design |
| **[Wait Calls Audit](wait-calls-audit-baseline.md)** | Wait call audit baseline |
| **[Worker Construction Logging](worker-construction-logging.md)** | Worker construction logging |
| **[Worker Construction Subprocess Analysis](worker-construction-subprocess-analysis.md)** | Worker subprocess analysis |
| **[Span Scope Preservation Design](span-scope-preservation-design.md)** | Span scoping design |
| **[Span Scoping Evaluation](span-scoping-patterns-evaluation.md)** | Span scoping evaluation |
| **[OTLP Resource Verification](otlp-resource-attribute-verification.md)** | OTLP resource verification |
| **[API Pattern Transformation](api-pattern-transformation-guide.md)** | API transformation patterns |
| **[ProcessGuard Coverage Catalog](processguard_coverage_catalog.md)** | ProcessGuard coverage |
| **[Release History Gap v0.2.13-v0.2.15](release-history-gap-v0.2.13-v0.2.15.md)** | Release history gap analysis |
| **[Release History Gap v0.2.13-v0.2.15](release-history-gap-v0.2.13-v0.2.15.md)** | Release history gap analysis |
| **[Retry Test Infrastructure](retry-test-infrastructure-guide.md)** | Retry test infrastructure |
| **[Test Isolation Inventory](test-isolation-inventory.md)** | Test isolation catalog |
| **[Test Isolation Audit Report](test-isolation-audit-report.md)** | Test isolation audit report |
| **[Test Isolation Catalog](test-isolation-catalog.md)** | Test isolation detailed catalog |
| **[Test Isolation Cross-Reference](test-isolation-cross-reference-checklist.md)** | Test isolation checklist |
| **[Test Isolation Comment Template](test-isolation-comment-template.md)** | Test isolation comment template |
| **[Testing Isolation Patterns](testing-isolation-patterns.md)** | Test isolation patterns |
| **[Testing Panic Safety](testing-panic-safety.md)** | Panic safety in tests |
| **[Testing Template System](testing/template-system-tests.md)** | Template system tests |
| **[Test Coverage: Log Level Verification](test-coverage/log-level-verification.md)** | Log level verification |

---

## Reference

Technical specifications, schemas, and reference documentation.

### Schemas & Specifications

| Document | Description |
|----------|-------------|
| **[Telemetry Event Schema](telemetry-event-schema.md)** | Complete telemetry event catalog |
| **[Telemetry Field Capture Strategy](telemetry-field-capture-strategy.md)** | How telemetry fields are captured |

### Verification Reports

| Document | Description |
|----------|-------------|
| **[Research: Mitosis Implementation](research-mitosis-implementation-and-failure-semantics.md)** | Mitosis failure semantics (alt location) |
| **[Mitosis Implementation](mitosis-implementation-and-failure-semantics.md)** | Mitosis implementation details |

### Reference Documentation

| Document | Description |
|----------|-------------|
| **[OTLP Collector Example](examples/otel-collector/README.md)** | Example OTLP Collector setup |

---

## Research & Notes

Background research, operational notes, and exploratory investigations.

### Research Documents

| Document | Description |
|----------|-------------|
| **[Agent Skills & Integrations](research/agent-skills-and-integrations.md)** | Agent integration patterns |
| **[Beads Fleet Dashboard](research/beads-fleet-dashboard.md)** | Dashboard design research |
| **[Beads Orchestration Claude](research/beads-orchestration-claude.md)** | Claude orchestration research |
| **[Beads-Polis Concurrent Fork](research/beads-polis-concurrent-fork.md)** | Concurrent fork patterns |
| **[Beads-Rust Native Workflow](research/beads-rust-native-workflow.md)** | Rust workflow patterns |
| **[Beads Workflow Gemini](research/beads-workflow-gemini.md)** | Gemini workflow patterns |
| **[BG Gate Validation](research/bg-gate-validation.md)** | Gate validation research |
| **[Concurrency Approaches Compared](research/concurrency-approaches-compared.md)** | Concurrency strategy comparison |
| **[Criterion Percentile Research](research/criterion-percentile-research.md)** | Percentile calculation research |
| **[Ecosystem Overview](research/ecosystem-overview.md)** | Beads ecosystem landscape |
| **[Mitosis Implementation](research/mitosis-implementation-and-failure-semantics.md)** | Mitosis research |
| **[OBC Multi-Agent Workflow](research/obc-multi-agent-workflow.md)** | Multi-agent patterns |
| **[Perles Orchestration](research/perles-orchestration-control-plane.md)** | Control plane research |
| **[Ralph Loop Pattern](research/ralph-loop-pattern.md)** | Loop patterns |
| **[Self-Learning Agents](research/self-learning-agents.md)** | Learning agent research |
| **[Spec2Beads Decomposition](research/spec2beads-decomposition.md)** | Task decomposition |
| **[Steve Yegge Beads Vision](research/steveyegge-beads-vision.md)** | Historical vision |

### 2026-08 Notes (Indexed in Investigations)

See [Investigations & Post-Mortems](#investigations--post-mortems) for:
- **[2026-08-29 Issue 16 Comment Loop](notes/2026-08-29-issue-16-comment-loop.md)** — GitHub issue #16 comment loop analysis
- **[Anthropic Routing Verification 2026-08-28](notes/anthropic-routing-verification-2026-08-28.md)** — Anthropic model routing verification
- **[GLM-4.7 Routing Verification 2026-08-28](notes/glm-4.7-routing-verification-2026-08-28.md)** — GLM-4.7 routing verification
- **[False Close Audit 2026-08](notes/false-close-audit-2026-08.md)** — False close analysis
- **[Pulse Strand Enablement 2026-08-15](notes/pulse-strand-enablement-2026-08-15.md)** — Pulse strand enablement analysis
- **[Bead-rs Fleet Migration 2026-08-15](notes/bead-rs-fleet-migration-2026-08-15.md)** — Bead-rs migration notes

### Operational Notes (Pre-2026-08)

| Document | Description |
|----------|-------------|
| **[Bash at Scale Problems](notes/bash-at-scale-problems.md)** | Bash scaling issues |
| **[Bead Lifecycle Bugs](notes/bead-lifecycle-bugs.md)** | Bead lifecycle bug catalog |
| **[Bundler Build Integrity](notes/bundler-build-integrity.md)** | Build integrity lessons |
| **[Claim Race Conditions](notes/claim-race-conditions.md)** | Race condition patterns |
| **[Explore Strand Bugs](notes/explore-strand-bugs.md)** | Explore strand issues |
| **[Mitosis Explosion Postmortem](notes/mitosis-explosion-postmortem.md)** | Mitosis failure analysis |
| **[Needle Binary Rollback](notes/needle-binary-rollback.md)** | Rollback procedures |
| **[Operational Fleet Lessons](notes/operational-fleet-lessons.md)** | Fleet operations lessons |
| **[OTLP Wire Capture](notes/otlp-wire-capture.md)** | OTLP debugging |
| **[Resource Awareness & Density](notes/resource-awareness-and-worker-density.md)** | Resource management |
| **[Routing Failure Results](notes/routing-failure-results.md)** | Routing failure patterns |
| **[Routing Telemetry Events](notes/routing-telemetry-events.md)** | Routing telemetry |
| **[Routing Testing](notes/routing-testing.md)** | Routing test procedures |
| **[Routing Test Results](notes/routing-test-results.md)** | Routing test outcomes |
| **[Self-Modification Risks](notes/self-modification-risks.md)** | Self-modification hazards |
| **[Worker Starvation Lessons](notes/worker-starvation-lessons.md)** | Starvation prevention |
| **[Idle Strand Gating Semantics](notes/idle-strand-gating-semantics.md)** | Idle gating behavior |
| **[Claude Print Routing Validation](notes/claude-print-routing-validation.md)** | Claude routing validation |
| **[Anthropic Routing Verification](notes/anthropic_routing_verification.md)** | Anthropic routing checks |
| **[BF-4utk](notes/bf-4utk.md)** | BF-4utk analysis |
| **[CI Sensor Jetstream Watchdog](notes/ci-sensor-jetstream-watchdog.md)** | CI monitoring |

### Operational Notes

| Document | Description |
|----------|-------------|
| **[Bash at Scale Problems](notes/bash-at-scale-problems.md)** | Bash scaling issues |
| **[Bead Lifecycle Bugs](notes/bead-lifecycle-bugs.md)** | Bead lifecycle bug catalog |
| **[Bundler Build Integrity](notes/bundler-build-integrity.md)** | Build integrity lessons |
| **[Claim Race Conditions](notes/claim-race-conditions.md)** | Race condition patterns |
| **[Explore Strand Bugs](notes/explore-strand-bugs.md)** | Explore strand issues |
| **[Mitosis Explosion Postmortem](notes/mitosis-explosion-postmortem.md)** | Mitosis failure analysis |
| **[Needle Binary Rollback](notes/needle-binary-rollback.md)** | Rollback procedures |
| **[Operational Fleet Lessons](notes/operational-fleet-lessons.md)** | Fleet operations lessons |
| **[OTLP Wire Capture](notes/otlp-wire-capture.md)** | OTLP debugging |
| **[Resource Awareness & Density](notes/resource-awareness-and-worker-density.md)** | Resource management |
| **[Routing Failure Results](notes/routing-failure-results.md)** | Routing failure patterns |
| **[Routing Telemetry Events](notes/routing-telemetry-events.md)** | Routing telemetry |
| **[Routing Testing](notes/routing-testing.md)** | Routing test procedures |
| **[Routing Test Results](notes/routing-test-results.md)** | Routing test outcomes |
| **[Self-Modification Risks](notes/self-modification-risks.md)** | Self-modification hazards |
| **[Worker Starvation Lessons](notes/worker-starvation-lessons.md)** | Starvation prevention |
| **[Idle Strand Gating Semantics](notes/idle-strand-gating-semantics.md)** | Idle gating behavior |
| **[Claude Print Routing Validation](notes/claude-print-routing-validation.md)** | Claude routing validation |
| **[Anthropic Routing Verification](notes/anthropic_routing_verification.md)** | Anthropic routing checks |
| **[BF-4utk](notes/bf-4utk.md)** | BF-4utk analysis |
| **[CI Sensor Jetstream Watchdog](notes/ci-sensor-jetstream-watchdog.md)** | CI monitoring |

---

## File Count Summary

- **Total markdown files:** 148
- **ADRs:** 21
- **Research documents:** 19
- **Operational notes:** 28
- **Investigations:** 40
- **Architecture & design:** 8
- **Operations & guides:** 27
- **Templates & examples:** 5

---

**Last updated:** 2026-08-29
