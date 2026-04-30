# Workspace Learnings

This file is automatically managed by NEEDLE. Learnings from completed beads are captured here.

### 2026-04-04 | bead: needle-wysd.2.3 | worker: bravo | type: other | reinforced: 419
- **Observation:** For new modules: create src/module/mod.rs, add to lib.rs, include tests in #[cfg(test)] module
- **Confidence:** high
- **Source:** reusable-pattern from needle-wysd.2.3

### 2026-04-04 | bead: needle-wysd.2.3 | worker: bravo | type: other | reinforced: 0
- **Observation:** Followed existing module structure patterns; Retrospective::parse_from_close_body handles edge cases with clean regex-free parsing
- **Confidence:** medium
- **Source:** what-worked from needle-wysd.2.3

### 2026-04-04 | bead: needle-wysd.2.3 | worker: bravo | type: other | reinforced: 0
- **Observation:** Initial import of unused Context triggered clippy warning; fixed by removing unused import
- **Confidence:** low
- **Source:** what-didnt-work from needle-wysd.2.3

### 2026-04-21 | bead: needle-49vt | worker: juliet | type: other | reinforced: 32
- **Observation:** N/A
- **Confidence:** low
- **Source:** what-didnt-work from needle-49vt

### 2026-04-21 | bead: needle-jy7b | worker: india | type: other | reinforced: 87
- **Observation:** Following existing ReflectConfig patterns (default functions, serde attributes) made integration seamless; all 858 tests passed on first run
- **Confidence:** medium
- **Source:** what-worked from needle-jy7b

### 2026-04-21 | bead: needle-jy7b | worker: india | type: other | reinforced: 0
- **Observation:** ReflectConfig already had comprehensive default value tests; added default_reflect_config_values for completeness
- **Confidence:** medium
- **Source:** surprise from needle-jy7b

### 2026-04-21 | bead: needle-jy7b | worker: india | type: other | reinforced: 0
- **Observation:** N/A - straightforward addition with no blockers
- **Confidence:** low
- **Source:** what-didnt-work from needle-jy7b

### 2026-04-21 | bead: needle-ry21.1 | worker: alpha | type: other | reinforced: 32
- **Observation:** Nothing failed; straightforward dependency addition.
- **Confidence:** low
- **Source:** what-didnt-work from needle-ry21.1

### 2026-04-26 | bead: needle-9hu7 | worker: charlie | type: other | reinforced: 3
- **Observation:** Found that the rate limiter uses the registry (which filters dead PIDs) for concurrency checks, so no explicit reconciliation is needed
- **Confidence:** medium
- **Source:** surprise from needle-9hu7

### 2026-04-26 | bead: needle-vqm7 | worker: india | type: other | reinforced: 3
- **Observation:** The telemetry infrastructure was already well-designed — the EventKind enum, event name mappings, and styling were all in place. Only the actual emission call was missing.
- **Confidence:** medium
- **Source:** what-worked from needle-vqm7

