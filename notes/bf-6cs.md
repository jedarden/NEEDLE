# Bead bf-6cs: Fleet CPU and Memory Telemetry Events

## Status: Already Implemented

This bead requested the implementation of structured telemetry events for fleet CPU saturation and memory low warnings. Upon investigation, all deliverables were already in place:

### Completed Implementation

1. **Event variants defined** (src/telemetry/mod.rs:271-279)
   - `FleetCpuSaturated { load_average, threshold, core_count }`
   - `FleetMemoryLow { free_mb, threshold_mb }`

2. **Event type mappings** (src/telemetry/mod.rs:657-658)
   - `"fleet.cpu_saturated"` for CPU saturation
   - `"fleet.memory_low"` for memory warnings

3. **Data serialization** (src/telemetry/mod.rs:1111-1130)
   - Both events properly serialize to JSON with all numeric values

4. **Emission in check_system_resources()** (src/rate_limit/mod.rs:350-409)
   - Function accepts `&Telemetry` sink
   - Emits `FleetCpuSaturated` when normalized load exceeds threshold
   - Emits `FleetMemoryLow` when free memory drops below threshold
   - Maintains tracing::warn! logs for operator visibility

5. **Call site updated** (src/worker/mod.rs:1477-1481)
   - Passes `&self.telemetry` to check_system_resources()

6. **Documentation updated** (docs/plan/plan.md:1674-1675)
   - Events catalogued in Health event catalog
   - Event types, fields, and types documented

### Verification

The implementation follows all requirements:
- ✅ Events appear in JSONL logs with numeric values
- ✅ Events are exported via OTLP sink (severity WARN)
- ✅ Design principle 4 honored: observable by default with structured telemetry

The bead is already complete and ready to close.
