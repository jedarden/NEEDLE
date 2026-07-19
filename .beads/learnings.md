# Workspace Learnings

This file is automatically managed by NEEDLE. Learnings from completed beads are captured here.

### 2026-07-16 | bead: session-a775af70-bd74-49dc-babc-9ce21ab7da73 | worker: needle | type: other | reinforced: 10930
- **Observation:** Action-outcome: Read → File read successfully (83905 bytes)
- **Confidence:** medium
- **Source:** transcript action-outcome: a775af70-bd74-49dc-babc-9ce21ab7da73

### 2026-07-16 | bead: session-a775af70-bd74-49dc-babc-9ce21ab7da73 | worker: needle | type: bug-fix | reinforced: 0
- **Observation:** Error pattern: bash: {"command":"timeout 120 cargo test --test integration_tests 2>&1 | tail -200","description":"Run ... — Exit code 143
- **Confidence:** high
- **Source:** transcript error: a775af70-bd74-49dc-babc-9ce21ab7da73

### 2026-07-16 | bead: session-a775af70-bd74-49dc-babc-9ce21ab7da73 | worker: needle | type: other | reinforced: 0
- **Observation:** Reasoning pattern: The user wants me to fix failing tests in `tests/integration_tests.rs`. Let me start by reading the file to see what tests exist and understand what might be failing.
- **Confidence:** low
- **Source:** transcript: a775af70-bd74-49dc-babc-9ce21ab7da73

### 2026-07-16 | bead: session-be2b626b-a48a-43fc-8e4c-9cdf2046b490 | worker: needle | type: feature | reinforced: 12
- **Observation:** Decision: The test `default_routing_rules_anthropic_subscription_models` which was running for over 60 seconds has now completed (ok)
- **Confidence:** medium
- **Source:** transcript decision: be2b626b-a48a-43fc-8e4c-9cdf2046b490
- **Decision ID:** dec-04a93ef99d58cfb5
- **Decision:** The test `default_routing_rules_anthropic_subscription_models` which was running for over 60 seconds has now completed (ok)
- **Context:** Good! I can see the tests are progressing.
- **Rationale:** 
- **Alternatives:** 60 seconds has now completed

### 2026-07-16 | bead: session-23e4155c-67c4-4b54-90b7-24cbfdf351b8 | worker: needle | type: feature | reinforced: 3
- **Observation:** Decision: They should use `--title=` instead of positional arguments:
- **Confidence:** medium
- **Source:** transcript decision: 23e4155c-67c4-4b54-90b7-24cbfdf351b8
- **Decision ID:** dec-279e35a4493e0c8c
- **Decision:** They should use `--title=` instead of positional arguments:
- **Context:** Now I need to fix all the br create commands in the tests.
- **Rationale:** 
- **Alternatives:** positional arguments:

### 2026-07-16 | bead: drift-drift-0 | worker: needle-drift | type: other | reinforced: 703
- **Observation:** Drift detected: Inconsistent approaches across similar sessions. Workers used different tools, file patterns, or action sequences for similar task types without clear temporal progression — may indicate need for standardization.
- **Confidence:** high
- **Source:** drift-cluster: drift-0

