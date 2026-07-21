# Sanitization Latency CI Configuration

This document describes the CI configuration for the sanitization latency assertion test.

## Overview

The sanitization latency assertion test validates that the sanitization pipeline meets the 10ms threshold for median latency on 100KB traces. This test runs automatically on every push to the main branch.

## Environment Variables

### SANITIZE_THRESHOLD_MS

The maximum allowed median latency in milliseconds for sanitizing a 100KB trace.

- **Default**: `10` (release builds), `500` (debug builds)
- **Purpose**: Enforces performance requirements for the sanitization pipeline
- **Location**: Set in the CI workflow template

### SANITIZER_BENCH_SAMPLE_COUNT

The number of iterations to run when measuring latency.

- **Default**: `50`
- **Purpose**: Higher sample count provides more stable median measurements
- **Usage**: Optional override for testing

## Test Location

The assertion test is located at:
```
tests/sanitize_latency_assertion.rs
```

## Running the Test Locally

```bash
# Run with default threshold (10ms release, 500ms debug)
cargo test sanitize_latency_below_threshold

# Run with custom threshold
SANITIZE_THRESHOLD_MS=25 cargo test sanitize_latency_below_threshold

# Run with output
cargo test --test sanitize_latency_assertion sanitize_latency_below_threshold -- --nocapture

# Run all sanitization latency tests
cargo test --test sanitize_latency_assertion
```

## CI Configuration

The test runs automatically in CI via the `needle-ci` workflow template:

```yaml
# declarative-config/k8s/iad-ci/argo-workflows/needle-workflowtemplate.yml
export SANITIZE_THRESHOLD_MS=${SANITIZE_THRESHOLD_MS:-10}
cargo test --lib
```

## Success Criterion

Phase 4 success criterion: sanitization must complete in <10ms per 100KB trace on a single core, with Aho-Corasick pre-filter demonstrably skipping irrelevant rules.

## Performance Metrics

The test reports the following metrics:
- **Min**: Minimum latency
- **Median**: Median latency (the value compared against the threshold)
- **Avg**: Average latency
- **P95**: 95th percentile latency
- **P99**: 99th percentile latency
- **Max**: Maximum latency

## Failing the Test

If the median latency exceeds the threshold, the test fails with:

```
Sanitizer median latency ({median_ms} ms) exceeds threshold ({threshold_ms} ms)
```

This indicates a performance regression that should be investigated before merging.
