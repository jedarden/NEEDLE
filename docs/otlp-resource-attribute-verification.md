# OTLP Resource Attribute Verification Guide

**Date:** 2026-08-29  
**Bead:** needle-aa054fd4  
**Purpose:** Document how to verify that NEEDLE workers export `process.owner` and other resource attributes correctly via OTLP

## Overview

NEEDLE workers export telemetry via OpenTelemetry Protocol (OTLP) to a collector. Resource attributes are metadata that describe the worker process itself (as opposed to per-record attributes which may vary per log/metric).

## Resource Attributes Set by NEEDLE

All NEEDLE workers export these resource attributes:

| Attribute | Example Value | Source | Fixed/Variable |
|-----------|---------------|--------|---------------|
| `service.name` | `needle` | Hardcoded | Fixed (process) |
| `service.namespace` | `needle-fleet` | Config `service_namespace` | Fixed (process) |
| `service.version` | `0.3.1` | `CARGO_PKG_VERSION` | Fixed (process) |
| `service.instance.id` | `claude-code-glm-4.7-armor-1` | Worker ID | Fixed (process) |
| `needle.session_id` | `de7fb055` | Session UUID | Fixed (process) |
| `host.name` | `ex44` | OS hostname | Fixed (process) |
| `process.pid` | `12345` | `std::process::id()` | Fixed (process) |
| **`process.owner`** | **`coding`** | **libc `getpwuid_r()`** | **Fixed (process)** |
| `deployment.cluster` | `ex44` | Config `resource_attributes` | Fixed (host) |
| `needle.worker.pool` | `bare-metal` | Config `resource_attributes` | Fixed (host) |
| `needle.agent` | `claude-anthropic-sonnet` | Config default | Resource default |
| `needle.model` | `claude-sonnet-4-6` | Config default | Resource default |
| `needle.workspace` | `NEEDLE` | Basename of workspace path | Resource default |

**Key Points:**
- **`process.owner`** is the username of the process owner, obtained via libc `getpwuid_r()` 
- Falls back to `uid:1000` format when passwd lookup fails (distroless containers)
- Is a **resource attribute** (appears on ALL spans/metrics/logs from the worker)
- Is **NOT** promoted to individual metric dimensions (stays at resource level only)

## Manual Verification

### Prerequisites

1. A running NEEDLE worker with OTLP enabled
2. Access to the OTLP collector or ability to run a local collector
3. `grpcurl` or similar tool for inspecting gRPC services

### Method 1: Local Collector Capture (Recommended for Testing)

This method runs a local OTLP collector and captures the raw wire format.

```bash
# 1. Create a collector config that writes to files
cat > /tmp/otel-collector-config.yaml <<'EOF'
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
      http:
        endpoint: 0.0.0.0:4318

processors:
  batch:

exporters:
  file:
    path: /tmp/otel-output/traces.json
    format: json

  file/metrics:
    path: /tmp/otel-output/metrics.json
    format: json

  file/logs:
    path: /tmp/otel-output/logs.json
    format: json

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [file]

    metrics:
      receivers: [otlp]
      processors: [batch]
      exporters: [file/metrics]

    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: [file/logs]
EOF

# 2. Start the collector
docker run -d --name otlp-collector \
  -p 4317:4317 -p 4318:4318 \
  -v /tmp/otel-collector-config.yaml:/etc/otelcol-contrib/config.yaml \
  -v /tmp/otel-output:/tmp/otel-output \
  otel/opentelemetry-collector-contrib:latest

# 3. Configure a test worker to use localhost
cat > /tmp/needle-test-config.yaml <<'EOF'
telemetry:
  otlp_sink:
    enabled: true
    endpoint: http://localhost:4318
    protocol: http
    timeout_secs: 10
    compression: none
    tls:
      insecure: true
    headers: []
    resource_attributes:
      - "deployment.cluster=ex44"
      - "needle.worker.pool=bare-metal"
    metrics_interval_secs: 10
    service_namespace: needle-fleet
    max_queue_size: 2048
EOF

# 4. Run a test worker (isolated HOME, minimal workspace)
mkdir -p /tmp/needle-test-workspace/.beads
HOME=/tmp/needle-test-home \
NEEDLE_CONFIG=/tmp/needle-test-config.yaml \
needle run -w /tmp/needle-test-workspace \
  --worker-id test-otlp-export \
  --session-id test-session-$(date +%s) \
  --once

# 5. Inspect the exported logs (contains resource attributes)
sleep 2  # Wait for export
docker exec otlp-collector cat /tmp/otel-output/logs.json | jq '.'
```

**Expected output** (simplified):
```json
{
  "resourceLogs": [
    {
      "resource": {
        "attributes": [
          {"key": "service.name", "value": {"stringValue": "needle"}},
          {"key": "process.owner", "value": {"stringValue": "coding"}},
          {"key": "deployment.cluster", "value": {"stringValue": "ex44"}},
          {"key": "needle.worker.pool", "value": {"stringValue": "bare-metal"}},
          // ... other resource attributes
        ]
      },
      "scopeLogs": [...]
    }
  ]
}
```

### Method 2: Production Collector Inspection

For production workers exporting to the live collector:

```bash
# Collector endpoint from config
COLLECTOR_HOST="needle-otel-ex44-apexalgo-iad-ts.ardenone.com:4318"

# If you have access to the collector logs or metrics
# Check VictoriaMetrics/Prometheus for resource attributes
curl -s "http://victoriametrics:8428/api/v1/label/__name__/values" | jq .

# Or query for a specific metric to see its labels
curl -s 'http://victoriametrics:8428/api/v1/query?query=needle_workers_active' | jq '.data.result[0].metric'
```

### Method 3: Running the Integration Test

The test suite includes an integration test that validates OTLP export:

```bash
cd /home/coding/NEEDLE

# Run the OTLP integration tests
cargo test --features integration otlp_integration

# Or run a specific test
cargo test --features integration test_otel_export_contains_resource_attributes
```

## Verification Checklist

Use this checklist to verify that `process.owner` and other resource attributes are correctly configured:

- [ ] Unit test passes: `cargo test build_resource_includes_process_owner`
- [ ] Resource attributes include `process.owner` (not empty, either username or `uid:NNNN`)
- [ ] Resource attributes include `deployment.cluster` from config
- [ ] Resource attributes include `needle.worker.pool` from config
- [ ] Integration test shows resource attributes on the wire (not just in builder output)
- [ ] Metrics do NOT have `process.owner` as a dimension (stays at resource level only)

## Troubleshooting

### `process.owner` is missing or empty

**Cause:** The libc `getpwuid_r()` lookup failed and the fallback didn't trigger.

**Check:**
```bash
# Run the unit test that covers the fallback
cargo test process_owner_fallback_to_numeric_uid

# Verify libc is available
ldd $(which needle) | grep libc
```

**Expected:** Fallback to `uid:1000` format in distroless containers.

### Resource attributes not appearing in exported data

**Cause:** The resilient exporter wrappers weren't forwarding `set_resource()` calls (fixed in ADR-016).

**Check:**
```bash
# Verify the fix is present
grep -A 5 "fn set_resource" /home/coding/NEEDLE/src/telemetry/otlp.rs

# Should see delegation to inner exporter via Arc::get_mut
```

### Metrics have high cardinality (series explosion)

**Cause:** `process.owner` or other unbounded attributes were promoted to metric dimensions.

**Check:**
1. Verify collector's `transform/metric_dimensions` configuration
2. Confirm it only includes bounded attributes like `deployment.cluster` and `needle.worker.pool`
3. Check that `process.owner` is NOT in the metric dimensions list

**Current state:** `transform/metric_dimensions` is configured in the external OpenTelemetry Collector and should NOT include `process.owner`.

## Related Documentation

- **ADR-016:** OTLP Resource Propagation and Roaming-Worker Identity - details the resource attribute architecture
- **Production OTLP Configuration:** `/docs/production-otlp-configuration-2026-08-15.md`
- **OTLP Source Code:** `/src/telemetry/otlp.rs` - Resource builder and exporter implementation
