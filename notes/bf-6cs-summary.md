# Bead bf-6cs: Fleet CPU and Memory Telemetry Events

## Status: Already Implemented

All deliverables from bead bf-6cs were already present in the codebase:

### Implementation Verification

1. **Event Variants** (`src/telemetry/mod.rs`):
   - `FleetCpuSaturated { load_average: f64, threshold: f64, core_count: usize }` (line 255)
   - `FleetMemoryLow { free_mb: u64, threshold_mb: u64 }` (line 260)

2. **Event Type Mappings** (`src/telemetry/mod.rs`):
   - `FleetCpuSaturated` → `"fleet.cpu_saturated"` (line 639)
   - `FleetMemoryLow` → `"fleet.memory_low"` (line 640)

3. **Data Serialization** (`src/telemetry/mod.rs`):
   - `FleetCpuSaturated.to_data()` returns `{load_average, threshold, core_count}` (lines 1067-1076)
   - `FleetMemoryLow.to_data()` returns `{free_mb, threshold_mb}` (lines 1078-1086)

4. **Emission in check_system_resources()** (`src/rate_limit/mod.rs`):
   - CPU saturation: emits `FleetCpuSaturated` when normalized load exceeds threshold (lines 364-377)
   - Memory low: emits `FleetMemoryLow` when free memory drops below threshold (lines 382-408)

5. **Worker Integration** (`src/worker/mod.rs`):
   - Calls `check_system_resources()` with telemetry sink (lines 1477-1481)

6. **OTLP Severity Mapping** (`src/telemetry/otlp.rs`):
   - Both events mapped to `Severity::Warn` (lines 1293-1298)

7. **Plan Documentation** (`docs/plan/plan.md`):
   - Events cataloged with data fields (lines 1674-1675)

### Acceptance Criteria Met

- [x] `fleet.cpu_saturated` event appears in JSONL log when load exceeds threshold
- [x] `fleet.memory_low` event appears in JSONL log when free RAM drops below threshold  
- [x] Both events carry the numeric values (actual vs threshold) in their `data` field
- [x] Events are exported via OTLP sink (severity WARN)

### Test Coverage

- `system_resource_check_does_not_panic` - Verifies function doesn't panic
- `test_severity_for_fleet_cpu_saturated_is_warn` - Verifies WARN severity
- `test_severity_for_fleet_memory_low_is_warn` - Verifies WARN severity

All tests pass.
