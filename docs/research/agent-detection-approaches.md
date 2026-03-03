# Agent Detection Module: Research and Comparison

**Related Beads:**
- nd-1ge (Original: Implement agent detection module) - **CLOSED/SUPERSEDED**
- nd-3d3 (Alternative: Research and document options) - **THIS DOCUMENT**
- nd-5cp (Alternative: Research and document options)

**Date:** 2026-03-02 (Updated 2026-03-03)
**Status:** Research Document - Implementation Complete

> **Note:** The original bead nd-1ge has been closed as superseded. The implementation in `src/onboarding/agents.sh` is complete and functional. This document serves as architectural reference for future enhancements.

---

## Executive Summary

This document provides a comprehensive analysis of different approaches for implementing the agent detection module in NEEDLE. The current implementation (`src/onboarding/agents.sh`) uses hardcoded bash associative arrays with case statements. This research evaluates alternative architectures to help inform future decisions.

---

## Current Implementation Analysis

### Implementation Location
- **File:** `src/onboarding/agents.sh`
- **Dependencies:** `src/lib/output.sh`, `src/lib/json.sh`, `src/lib/constants.sh`

### Current Architecture

```bash
# Hardcoded agent registry using bash associative arrays
declare -A NEEDLE_AGENT_CMDS=(
    [claude]="claude"
    [opencode]="opencode"
    [codex]="codex"
    [aider]="aider"
)

# Case-based version detection
_needle_agent_version() {
    case "$agent" in
        claude) version=$(claude --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+') ;;
        opencode) version=$(opencode --version 2>/dev/null | ...) ;;
        # ...
    esac
}
```

### Current Strengths
1. **No external dependencies** - Pure bash implementation
2. **Fast execution** - No YAML parsing overhead
3. **Simple code flow** - Easy to understand and debug
4. **Works without agent configs** - Detection independent of YAML files
5. **Comprehensive auth checking** - Multiple fallback methods per agent

### Current Limitations
1. **Hardcoded agents** - Adding new agents requires code changes
2. **Duplicate definitions** - Agent info exists in both `agents.sh` and YAML configs
3. **Maintenance burden** - Version/auth logic must be updated per agent
4. **No plugin architecture** - Cannot extend without modifying source

---

## Alternative Approaches

### Approach 1: YAML-Driven Agent Registry (Recommended)

**Concept:** Define agent detection metadata in YAML configuration files alongside invocation configs.

#### Implementation Overview

```yaml
# config/agents/claude-anthropic-sonnet.yaml (extended)
name: claude-anthropic-sonnet
runner: claude

# Detection configuration
detection:
  command: claude           # Command to check
  version_flag: --version   # Flag to get version
  version_regex: '[0-9]+\.[0-9]+\.[0-9]+'

  # Auth detection methods (tried in order)
  auth:
    - method: cli
      command: claude auth status
      success_pattern: "logged in"
    - method: env
      variable: ANTHROPIC_API_KEY

  # Install instructions
  install:
    method: npm
    command: npm install -g @anthropic-ai/claude-code
```

#### Architecture

```
src/onboarding/agents.sh
    ├─ _needle_scan_agents()
    │     ├─ _needle_list_available_agents()  # From loader.sh
    │     ├─ _needle_load_agent(name)         # Load YAML
    │     └─ _needle_detect_from_yaml(config) # Use detection block
    │
    └─ _needle_detect_from_yaml()
          ├─ Parse detection.version_flag
          ├─ Execute: ${runner} ${version_flag}
          ├─ Parse with detection.version_regex
          └─ Check auth methods in order
```

#### Pros
- **Single source of truth** - Agent metadata in one place
- **Extensibility** - Add agents without code changes
- **Consistency** - Detection matches invocation config
- **User customization** - Users can add custom agents in `~/.needle/agents/`

#### Cons
- **YAML dependency** - Requires yq or Python
- **Slower startup** - Must parse YAML for each agent
- **Complexity** - More complex codebase
- **Backward compatibility** - Need fallback for missing detection config

#### Implementation Effort
- **Estimated:** 4-6 hours
- **Files Modified:** `src/onboarding/agents.sh`, all `config/agents/*.yaml`
- **Testing:** Moderate - need to test YAML parsing edge cases

---

### Approach 2: External Agent Manifest

**Concept:** Separate agent detection into a standalone JSON/YAML manifest file.

#### Implementation Overview

```yaml
# config/agent-manifest.yaml
version: "1.0"
agents:
  claude:
    display_name: Claude Code
    command: claude
    version_flag: --version
    version_regex: '[0-9]+\.[0-9]+\.[0-9]+'
    install:
      npm: @anthropic-ai/claude-code
    auth:
      cli: claude auth status
      env: ANTHROPIC_API_KEY

  opencode:
    display_name: OpenCode
    command: opencode
    # ...
```

#### Architecture

```
config/agent-manifest.yaml     # Single manifest file
src/onboarding/agents.sh       # Reads from manifest
src/lib/manifest.sh            # Manifest parsing utilities
```

#### Pros
- **Centralized** - All agents in one file
- **Easy maintenance** - Add/remove agents in one place
- **Fast loading** - Single file parse vs. multiple YAMLs
- **Clean separation** - Detection separate from invocation

#### Cons
- **Sync burden** - Must keep manifest in sync with agent configs
- **Single point of failure** - Corrupted manifest breaks detection
- **No user extensions** - Harder to add custom agents
- **Duplication** - Runner info duplicated between manifest and agent config

#### Implementation Effort
- **Estimated:** 3-4 hours
- **Files Modified:** `src/onboarding/agents.sh`, new `config/agent-manifest.yaml`
- **Testing:** Low - single file parsing

---

### Approach 3: Hybrid Approach with Convention

**Concept:** Use naming convention to link detection to YAML configs, with hardcoded fallback.

#### Implementation Overview

```bash
# Naming convention: <runner>-<provider>-<model>
# Example: claude-anthropic-sonnet -> runner is "claude"

_needle_detect_agent() {
    local agent_name="$1"

    # Extract runner from agent name (convention)
    local runner="${agent_name%%-*}"  # claude-anthropic-sonnet -> claude

    # Try YAML config first
    if _needle_load_agent "$agent_name" &>/dev/null; then
        # Use detection info from YAML if available
        _needle_detect_from_yaml "$runner"
    else
        # Fall back to hardcoded detection
        _needle_detect_hardcoded "$runner"
    fi
}
```

#### Architecture

```
src/onboarding/agents.sh
    ├─ Hardcoded NEEDLE_AGENT_CMDS (fallback)
    ├─ Hardcoded version/auth logic (fallback)
    │
    └─ _needle_detect_agent()
          ├─ Try YAML detection first
          └─ Fall back to hardcoded if YAML missing
```

#### Pros
- **Best of both worlds** - YAML when available, hardcoded fallback
- **Graceful degradation** - Works without YAML configs
- **Incremental migration** - Can migrate agents one at a time
- **No breaking changes** - Existing behavior preserved

#### Cons
- **Code duplication** - Two detection paths to maintain
- **Inconsistent behavior** - Different results depending on YAML presence
- **Debugging complexity** - Must understand which path was used
- **Partial solution** - Doesn't fully eliminate hardcoded agents

#### Implementation Effort
- **Estimated:** 2-3 hours
- **Files Modified:** `src/onboarding/agents.sh`
- **Testing:** Moderate - must test both paths

---

### Approach 4: Plugin Architecture with Executable Detectors

**Concept:** Each agent provides a detector script that outputs structured data.

#### Implementation Overview

```bash
# config/agents/detectors/claude.sh
#!/usr/bin/env bash
# Outputs JSON for Claude Code detection

check_installed() {
    command -v claude &>/dev/null || return 1
}

get_version() {
    claude --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1
}

check_auth() {
    if claude auth status 2>/dev/null | grep -qi "logged in"; then
        echo "authenticated"
    elif [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
        echo "authenticated"
    else
        echo "auth-required"
    fi
}

# Output JSON
if check_installed; then
    cat <<EOF
{
    "installed": true,
    "version": "$(get_version)",
    "auth_status": "$(check_auth)"
}
EOF
else
    echo '{"installed": false}'
fi
```

#### Architecture

```
config/agents/detectors/
    ├─ claude.sh
    ├─ opencode.sh
    ├─ codex.sh
    └─ aider.sh

src/onboarding/agents.sh
    └─ _needle_scan_agents()
          └─ For each detector in detectors/
                └─ Execute detector, parse JSON
```

#### Pros
- **Maximum flexibility** - Each agent can have complex detection logic
- **Language agnostic** - Detectors can be bash, Python, etc.
- **Community contributions** - Easy to add new detectors
- **Isolation** - Detector bugs don't affect other agents

#### Cons
- **Process overhead** - Spawning multiple subprocesses
- **Security concerns** - Executing external scripts
- **Distribution complexity** - Must bundle detectors with NEEDLE
- **Error handling** - Must handle detector failures gracefully

#### Implementation Effort
- **Estimated:** 6-8 hours
- **Files Created:** New `config/agents/detectors/*.sh` for each agent
- **Testing:** High - must test detector scripts independently

---

### Approach 5: Capability Probing (Auto-Discovery)

**Concept:** Probe potential commands to discover agents dynamically, no hardcoded list.

#### Implementation Overview

```bash
# List of potential agent commands to probe
NEEDLE_KNOWN_AGENTS=(
    "claude"
    "opencode"
    "codex"
    "aider"
    "gemini"
    "copilot"
    # ... extendable
)

_needle_probe_agents() {
    for cmd in "${NEEDLE_KNOWN_AGENTS[@]}"; do
        if command -v "$cmd" &>/dev/null; then
            # Try common version flags
            for flag in "--version" "-V" "version"; do
                output=$("$cmd" "$flag" 2>&1) && break
            done

            # Determine agent type by probing behavior
            _needle_identify_agent "$cmd" "$output"
        fi
    done
}

_needle_identify_agent() {
    local cmd="$1"
    local output="$2"

    # Identify by output patterns
    case "$output" in
        *Claude*Code*|*Anthropic*) echo "claude" ;;
        *OpenCode*|*opencode*)     echo "opencode" ;;
        *Codex*|*OpenAI*)          echo "codex" ;;
        *Aider*|*aider*)           echo "aider" ;;
        *)                         echo "unknown:$cmd" ;;
    esac
}
```

#### Architecture

```
src/onboarding/agents.sh
    ├─ NEEDLE_KNOWN_AGENTS array (minimal)
    └─ _needle_probe_agents()
          ├─ Check command existence
          ├─ Probe version flags
          └─ Identify by output patterns
```

#### Pros
- **Dynamic discovery** - Finds agents without exact knowledge
- **Future-proof** - Automatically handles new agent versions
- **Minimal configuration** - Just a list of commands
- **Handles unknown agents** - Can report "unknown" agents

#### Cons
- **Unreliable identification** - Output patterns may change
- **No install guidance** - Can't provide install commands for unknown agents
- **Version detection fragile** - Version flags vary between tools
- **Auth detection impossible** - Can't auto-detect auth for unknown agents

#### Implementation Effort
- **Estimated:** 4-5 hours
- **Files Modified:** `src/onboarding/agents.sh`
- **Testing:** High - must test with various tool versions

---

## Comparison Matrix

| Criteria | Current | YAML-Driven | Manifest | Hybrid | Plugin | Probing |
|----------|---------|-------------|----------|--------|--------|---------|
| **Extensibility** | ⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |
| **Performance** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐ |
| **Maintainability** | ⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ |
| **Reliability** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ |
| **User Customization** | ⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ |
| **Implementation Effort** | ✅ Done | 4-6h | 3-4h | 2-3h | 6-8h | 4-5h |
| **Dependencies** | None | yq/Python | yq/Python | yq/Python | None | None |

---

## Recommendation

Based on the analysis, the **Hybrid Approach (Approach 3)** is recommended for the following reasons:

1. **Low Risk** - Preserves existing behavior, adds YAML support incrementally
2. **Backward Compatible** - Works without any YAML configuration
3. **Extensible** - Users can add custom agents via YAML
4. **Reasonable Effort** - 2-3 hours implementation
5. **Migration Path** - Can evolve toward full YAML-driven approach over time

### Migration Strategy

**Phase 1: Add YAML Detection Support** (Immediate)
- Add optional detection block to YAML configs
- Modify `_needle_detect_agent()` to check YAML first
- Fall back to hardcoded logic if YAML missing

**Phase 2: Migrate Built-in Agents** (Short-term)
- Add detection config to all built-in agent YAMLs
- Test thoroughly with YAML-driven detection
- Keep hardcoded fallback as safety net

**Phase 3: Deprecate Hardcoded Logic** (Long-term)
- Once YAML detection is stable, remove hardcoded agents
- Add warning when using fallback
- Eventually remove fallback entirely

---

## Implementation Notes

### For Approach 3 (Hybrid), Required Changes:

1. **Extend YAML Schema** - Add `detection:` block to agent configs
2. **Modify `src/onboarding/agents.sh`**:
   - Add `_needle_detect_from_yaml()` function
   - Modify `_needle_detect_agent()` to try YAML first
   - Keep existing case statements as fallback

### Example Extended YAML

```yaml
# config/agents/claude-anthropic-sonnet.yaml
name: claude-anthropic-sonnet
runner: claude
provider: anthropic
model: sonnet

# Detection configuration (optional)
detection:
  version:
    flag: --version
    regex: '[0-9]+\.[0-9]+\.[0-9]+'
  auth:
    methods:
      - type: cli
        command: claude auth status
        success_pattern: "logged in"
      - type: env
        variable: ANTHROPIC_API_KEY
  install:
    method: npm
    command: npm install -g @anthropic-ai/claude-code
    docs: https://docs.anthropic.com/claude-code

# ... rest of config
```

---

## Open Questions

1. **Should detection include installation?**
   - Current: Yes, NEEDLE_AGENT_INSTALL provides install commands
   - Alternative: Defer to package managers or external docs

2. **How to handle agent aliases?**
   - Example: `claude-sonnet` vs `claude-anthropic-sonnet`
   - Need alias support in YAML or naming convention

3. **Should detection be cached?**
   - Expensive to probe all agents on every `needle init`
   - Cache in `~/.needle/cache/agent-status.json`?
   - TTL-based expiration?

4. **How to detect custom agents?**
   - User-defined agents in `~/.needle/agents/`
   - Require `detection:` block for custom agents?

---

## Related Files

- `src/onboarding/agents.sh` - Current implementation
- `src/agent/loader.sh` - YAML loading utilities
- `config/agents/*.yaml` - Agent configuration files
- `docs/plan.md` - Overall project architecture

---

## References

- [NEEDLE Plan Document](../docs/plan.md) - Section on Agent Detection
- [Beads Rust Integration](../docs/plan.md#dependencies) - Atomic claiming
- [Agent Configuration Schema](../config/agents/) - Existing YAML configs
