# Test Fixtures and Mock Helpers for Workspace Scenarios

## Task: bf-t9ya2

### Summary
Test infrastructure for workspace scenarios already fully implemented in `tests/workspace_fixtures.rs`.

## Implementation Status

### ✅ Mock Workspace Structures
- `MockWorkspace` struct with path and state
- `WorkspaceState` enum (Dead/Alive)
- `MockCandidate` struct with controlled states
- `CandidateState` enum (Excluded/Assigned/Claimable)

### ✅ Helper Functions for Test Scenarios
- `deadlock_scenario()` - Creates ws1 (dead) + ws2 (alive)
- `all_dead_scenario()` - All workspaces with no claimable candidates
- `all_alive_scenario()` - All workspaces with valid candidates
- `mixed_states_scenario()` - Single workspace with mixed states
- `ScenarioBuilder` - Custom scenario builder pattern

### ✅ Mock BeadStore
- `MockCandidateStore` with configurable behavior
- Supports empty, failing, and custom candidate states
- Full `BeadStore` trait implementation

### ✅ Utility Functions
- `count_candidates_by_state()` - Count by candidate state
- `candidates_for_workspace()` - Filter by workspace
- `candidates_by_state()` - Filter by state
- `candidates_to_beads()` - Convert to Bead structs
- `test_components()` - Standard test setup (registry, telemetry, worker_id)

### ✅ Test Coverage
All 8 tests passing:
- `test_deadlock_scenario_structure` - Verifies deadlock scenario structure
- `test_all_dead_scenario` - All workspaces dead
- `test_all_alive_scenario` - All workspaces alive
- `test_mixed_states_scenario` - Mixed states in single workspace
- `test_scenario_builder` - Builder pattern works
- `test_candidate_to_bead_conversion` - Conversion logic
- `test_filter_candidates` - Filtering utilities
- `test_mock_candidate_store` - Mock store async operations

## Usage Example

```rust
use workspace_fixtures::*;

// Create the classic deadlock scenario
let (workspaces, candidates, home) = deadlock_scenario();

// Verify structure
assert_eq!(workspaces.len(), 2);

let (assigned, excluded, claimable) = count_candidates_by_state(&candidates);
assert_eq!(assigned, 2);    // ws1 has 2 assigned
assert_eq!(excluded, 2);    // ws1 has 2 excluded
assert_eq!(claimable, 2);   // ws2 has 2 claimable

// Use in tests with mock store
let store = MockCandidateStore::new(candidates_to_beads(&candidates));
```

## Acceptance Criteria
- ✅ Test module structure in place
- ✅ Mock structs and helpers working (8 tests passing)
- ✅ Can create the specific deadlock scenario (ws1 dead, ws2 alive)

## Notes
The explore strand tests that use these fixtures (`deadlock_scenario_assigned_beads_allow_advancement`, `deadlock_scenario_excluded_beads_allow_advancement`) are currently failing, but those are testing the actual `ExploreStrand` implementation logic, not the test fixture infrastructure itself. The fixtures are working correctly and can create all required scenarios.
