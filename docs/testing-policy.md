# Test tiers and growth budgets

`scripts/check-test-policy.sh` is the executable authority for the structure
of NEEDLE's test suite. Its declaration lives in `tests/test-policy.toml`.

Every Cargo integration harness has exactly one tier:

- `push`: deterministic in-process coverage that belongs on every push.
- `contract`: compatibility with an external executable or API.
- `e2e`: process-level or cross-component behavior.
- `soak`: long-running reliability checks.
- `fuzz`: generated-input or property exploration outside the push lane.

The policy records a test-count ceiling for every harness, every tier in
aggregate, and the unit tier. A ceiling is a review gate: exceeding it means
either moving the test to a cheaper or more accurate tier, or deliberately
raising both the harness and aggregate budgets. Deleting tests never requires
a policy edit. Counts use literal `#[test]` and `#[tokio::test]` attributes so
the check is fast, offline, and independent of compilation.

Cargo integration auto-discovery stays disabled. A top-level `tests/*.rs`
file must therefore be both an explicit Cargo target and a policy entry; a
loose Rust file is rejected instead of silently becoming dead coverage.
Non-harness files under `tests/` are verification assets and must match an
artifact rule with a component owner.

Unit tests may not add raw wall-clock sleeps, child-process construction, or
process-global environment mutation. Those operations make the most heavily
parallelized tier slow or order-dependent. Existing debt is represented by
per-file, per-hazard exception budgets in the policy. Every exception has an
owner and reason, cannot grow silently, and should be removed when the test is
moved or replaced with an injected clock, runner, or scoped environment.

Run the policy check directly with:

```bash
scripts/check-test-policy.sh
```

It also runs in the Definition of Done fast lane, before any Rust compilation.
