# Unit Test Results: prompt and dispatch Modules

## Date
2026-07-14

## Summary
Ran unit tests for both `prompt` and `dispatch` modules together. All tests passed successfully.

## Test Results
- **Total tests run:** 93
- **Passed:** 93
- **Failed:** 0
- **Ignored:** 0
- **Test duration:** 3.04s

## prompt Module Tests (28 tests)
All tests in `src/prompt/mod.rs` passed:

- Template rendering and variable substitution
- All five default templates (pluck, split, mitosis, weave, unravel, pulse)
- Deterministic output for same inputs
- Hash computation (SHA-256)
- Token estimation
- Context file loading
- Template validation (unknown variables, strand-specific vars)
- Config template overrides
- Escaped brace handling
- Extra variable substitution for strand-specific templates

## dispatch Module Tests (65 tests)
All tests in `src/dispatch/mod.rs` passed:

- Template rendering with variable substitution
- AgentAdapter YAML serialization/deserialization
- Built-in adapter configurations (claude-sonnet, claude-opus, opencode, codex, aider, generic)
- Effective timeout calculation
- Adapter loading from YAML directory
- User adapter overrides
- Temp file creation and cleanup
- Process dispatch integration (stdout/stderr capture, exit codes)
- Timeout enforcement (returns 124)
- Environment variable injection
- Token extraction (JSON field and regex methods)
- E2E tests: template variables, environment, shell metacharacters, newlines
- E2E tests: exit codes (0, 1, 2, 137), timeout, process group kill
- E2E tests: JSON output capture, token extraction, workspace directory
- GenAI semantic conventions (provider, model, gen_ai_system)

## Dependencies
Both modules share similar dependencies:
- `types` - core bead and outcome types
- `config` - configuration structures
- `telemetry` - event emission
- Additional: `prompt` uses `skill` and `learning`; `dispatch` uses `trace` and `sanitize`

## Conclusion
No failing tests for either module. Both modules are functioning correctly.
