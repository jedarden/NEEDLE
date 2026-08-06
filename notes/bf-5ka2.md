# Bead bf-5ka2: Routing Tests and Documentation

## Task Summary
Write routing tests and update documentation with routing policy and June 15 rationale.

## Status: COMPLETE ✅

## Acceptance Criteria Verification

### ✅ Integration tests pass (cargo test)
- **File**: `tests/routing_integration.rs` (1,969 lines)
- **Tests**: 41 routing integration tests, all passing
- **Coverage**:
  - Anthropic Claude models → claude-print (sonnet, opus, fable, haiku)
  - GLM models → claude-code-glm-4.7
  - Workspace override of global routing rules
  - Missing adapter = loud failure
  - First-match-wins semantics
  - Strict vs non-strict modes
  - Real-world configuration testing

### ✅ docs/plan.md documents routing policy and June 15 deadline
- **Location**: `docs/plan/plan.md` lines 717-810
- **Sections**:
  - "Model-based adapter routing" (lines 717-765)
  - "Anthropic Subscription Billing Policy (Pre-June 15, 2026)" (lines 766-810)
- **Content**:
  - June 15, 2026 deadline rationale
  - Routing policy explanation
  - Subscription value maximization strategy
  - Cost optimization reasoning

### ✅ Example .needle.yaml snippet in docs
- **Location**: `docs/plan/plan.md` line 791-808
- **Content**: Complete YAML configuration example showing:
  ```yaml
  agent:
    default: claude
    timeout: 3600
    routing:
      rules:
        - match_model: "(claude-)?(sonnet|opus).*"
          adapter: claude-print
        - match_model: "(claude-)?(fable|haiku).*"
          adapter: claude-print
        - match_model: "glm-.*"
          adapter: claude-code-glm-4.7
      default_adapter: claude-code-glm-4.7
      strict: false
  ```

### ✅ All tests green (cargo + clippy)
- **Cargo test**: 41/41 routing tests pass (0.16s)
- **Clippy**: No warnings or errors for routing code
- **Coverage**: Comprehensive test suite with helper functions

## Implementation Details

The routing integration tests were already complete and comprehensive:

1. **Test Helpers**: Well-structured helper functions for creating test configs and dispatchers
2. **Anthropic Models**: Tests for sonnet, opus, fable, haiku routing to claude-print
3. **GLM Models**: Tests for glm-4.7 routing to claude-code-glm-4.7
4. **Dispatcher Integration**: End-to-end tests with real Dispatcher instances
5. **Workspace Overrides**: Tests for workspace-specific routing rules
6. **Missing Adapter**: Tests for loud failure when adapters don't exist
7. **First-Match-Wins**: Tests for ordered pattern matching semantics
8. **Real-World Configs**: Tests documenting the Anthropic subscription policy

## Historical Context

The routing feature was implemented to support Anthropic's subscription billing policy before the June 15, 2026 deadline. The feature shipped before this deadline (tracked by bead bf-2xi) to enable workspace operators to maximize subscription credit value.

## Conclusion

All acceptance criteria have been verified and met. The routing integration tests are comprehensive, the documentation is complete with the June 15 rationale and configuration examples, and all tests pass cleanly.
