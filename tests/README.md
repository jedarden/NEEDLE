# NEEDLE Integration Tests

This directory contains integration tests for the NEEDLE project.

## Known Test Environment Issues

### uncommitted_dependency_detection Tests (4 tests)

The following tests may exhibit different behavior on local development machines (ex44) compared to CI:

- `uncommitted_dependency_detection_clean_fails_workspace_passes`
- `uncommitted_dependency_detection_both_modes_pass`
- `uncommitted_dependency_detection_both_modes_fail`
- `uncommitted_dependency_detection_workspace_mode_bypasses_clean`

**Issue:** These tests spawn `cargo` commands. On the ex44 development machine, the `~/.local/bin/cargo` shim intercepts these calls and redirects them to remote CI execution. This environmental difference causes the tests to behave differently than on CI where no such shim exists.

**Status:** Not a code bug - these tests validate legitimate functionality (uncommitted dependency detection). The tests pass correctly on CI. The difference is purely environmental due to the cargo wrapper on ex44.

**Action:** When running tests locally, be aware that these tests may fail or produce unexpected results due to the cargo shim. This is expected and does not indicate a problem with the codebase.
