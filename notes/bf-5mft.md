# bead bf-5mft: Routing Config Schema

## Task
Add routing config schema for model-based adapter routing.

## Finding
The routing config schema was already fully implemented in `src/config/mod.rs`:

### Structures Present
- `RoutingRule` (lines 28-41): Has `match_model` and `adapter` fields
- `RoutingConfig` (lines 44-67): Has `rules: Vec<RoutingRule>`, `default_adapter: Option<String>`, `strict: bool`
- `AgentConfig.routing: Option<RoutingConfig>` (line 90)

### Implementation Features
1. **Default routing** (lines 121-132): Anthropic models → claude-print, others → claude-code-glm-4.7
2. **Workspace overrides** (line 1575): `WorkspaceAgentOverrides.routing` supports per-workspace routing
3. **Config merging** (lines 1766-1769): Workspace routing rules merge with global config
4. **Validation** (lines 2078-2096): Regex pattern validation for match_model fields
5. **Telemetry events** (telemetry/mod.rs lines 210-223): `RoutingDecision`, `RoutingFailed` for runtime routing

### Test Coverage (14 tests passing)
- `routing_config_with_rules_parses`: YAML parsing
- `routing_config_empty_rules_list_is_valid`: Graceful handling of empty rules
- `routing_config_backward_compatibility`: Missing routing uses defaults
- `routing_config_from_global_config_file`: Global config loading
- `routing_workspace_override`: Workspace override merging
- `routing_default_anthropic_models_to_claude_print`: Default rule validation
- `routing_first_match_wins`: First-match-wins behavior
- `routing_strict_mode_failure`: Strict mode validation
- `routing_patterns_match_*`: Various pattern matching tests
- `routing_config_yaml_roundtrip`: Serialization/deserialization

### Acceptance Criteria Status
- ✅ Config struct includes routing and default_adapter fields
- ✅ YAML parsing handles empty/missing routing gracefully
- ✅ Workspace .needle.yaml routing rules merge with global config
- ✅ cargo build succeeds

## Conclusion
No implementation work required. The feature is complete and tested.
