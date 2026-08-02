# Investigation Report: bf-3e4do (Claude Process Leak Incident)

## Incident Summary

**Date:** 2026-07-29 ~13:35 EDT
**Location:** lab (100.81.129.38)
**Symptom:** ~115 leaked 'claude' processes with cwd=$HOME, accumulated over 11-13 hours
**Load Impact:** Load average 304-308 on a 12-core box (procs.r: 281-295, 100% kernel time)

## Investigation Timeline

### Code Path Analysis

1. **Worker Spawn Flow** (`src/supervisor/mod.rs`):
   - `spawn_worker()` creates: `needle run --workspace <config.workspace> --agent <agent> --identifier <id> --count 1`
   - Workspace comes from `SupervisorConfig.workspace` 
   - Uses resolved `worker_binary` path (fix for GH #11, uses `current_exe()` not PATH lookup)

2. **Dispatch Flow** (`src/dispatch/mod.rs`):
   - Templates include explicit `cd {workspace}`:
     ```rust
     invoke_template: concat!(
         "cd {workspace} && unbuffer -p claude --model claude-sonnet-4-6",
         " --max-turns 30 --output-format stream-json --dangerously-skip-permissions",
         " --verbose < {prompt_file}",
     )
     ```
   - `run_process()` executes via: `bash -c "<rendered_template>"`
   - This should NEVER leave cwd at $HOME unless `{workspace}` is empty or resolves to $HOME

3. **Workspace Resolution** (`src/worker/mod.rs`):
   ```rust
   let dispatch_ws = if is_workspace_unset(&bead.workspace) {
       &self.config.workspace.default
   } else {
       &bead.workspace
   };
   ```
   
   Where `is_workspace_unset()` returns true if:
   ```rust
   fn is_workspace_unset(path: &std::path::Path) -> bool {
       let s = path.as_os_str();
       s.is_empty() || s == "."
   }
   ```

4. **Config Default** (`src/config/mod.rs`):
   ```rust
   fn default_workspace() -> PathBuf {
       std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
   }
   ```

### Root Cause Hypotheses

#### Hypothesis 1: Empty Workspace Variable (UNLIKELY)
- If `bead.workspace` is empty or "." and `config.workspace.default` is also empty/current_dir=$HOME
- **Counter-evidence:** Workers spawn with explicit `--workspace` arg, so default shouldn't be $HOME
- **Need to verify:** What was the actual config.workspace at supervisor launch?

#### Hypothesis 2: Explore Strand Workspace Root (PLAUSIBLE)
- `src/strand/explore.rs` uses `config.workspace_root` which defaults to `$HOME`
- Explore discovers workspaces under workspace_root by looking for `.beads/` directories
- If a bead was discovered via Explore but workspace resolution failed, could fall back to $HOME
- **Need to verify:** Was Explore running? What was workspace_root configured to?

#### Hypothesis 3: Malicious/Compromised Process (POSSIBLE)
- Day before (2026-07-28), there was a contamination incident:
  - Orphaned `integration_tests` process (PID 1834442) running for 11.5 hours
  - Claimed beads under fake identity "echo-test-test-worker" 
  - Created "trivial 0ms done traces"
  - Reset 16 affected beads via direct SQL
- **Similarities:** Both involved long-running phantom processes claiming work
- **Difference:** Integration_tests was a test binary, not a claude process

#### Hypothesis 4: Supervisor/Worker Desynchronization (NEEDS EVIDENCE)
- If supervisor spawned workers with incorrect workspace argument
- Or if workspace config was modified while supervisor was running
- **Need to verify:** Supervisor logs, config file state, launch arguments

#### Hypothesis 5: Template Rendering Failure (NEEDS EVIDENCE)
- If `{workspace}` template variable rendered as empty string
- Would result in command: `cd  && unbuffer -p claude ...` (cd with no arg stays in current dir)
- **Need to verify:** Template rendering logic, any known edge cases

### Open Research Questions from Bead

1. ✅ **What code path spawns 'claude' with cwd=$HOME?**
   - **Answer:** Only dispatch via `bash -c "cd {workspace} && ..."`
   - **Condition:** Requires `{workspace}` to be empty or resolve to $HOME

2. ❓ **Why did ~115 accumulate over ~11-13h?**
   - **Rate:** ~10-11 processes/hour
   - **Pattern:** Groups with identical spawn timestamps (programmatic mass-spawn)
   - **Need to check:** Is there a retry loop that spawns without checking previous attempt?

3. ❓ **What were these processes doing/blocked on?**
   - **Elapsed:** 41735-48134s (11.6-13.4 hours) - implies they were waiting/blocking
   - **Need to check:** Do logs exist for this time window? Any evidence of actual dispatch attempts?

4. ❓ **Is this reproducible?**
   - **Need to test:** Can we construct a scenario where workspace resolves to $HOME?

### Evidence Collection Needed

1. **Logs from 2026-07-29 00:48-13:35 EDT:**
   - `~/.needle/logs/needle-*.agent.jsonl` for active workers
   - Supervisor telemetry showing spawn decisions
   - Any stderr/stdout from the leaked processes

2. **Config State at Incident Time:**
   - What was `workspace.workspace_root` set to?
   - What was `workspace.default` set to?
   - Was Explore strand enabled?

3. **Process Tree Evidence:**
   - PPID of the 115 claude processes (all showed tmux server PPID)
   - This suggests they were spawned from tmux sessions (normal for needle run)

4. **Correlation with Integration_Tests Incident:**
   - Are the two incidents related?
   - Same attack vector?
   - Or systemic issue with process management?

### Preliminary Conclusions

1. **The code path is clear:** Processes are spawned via dispatch templates with explicit `cd {workspace}`
2. **For cwd to be $HOME:** Either `{workspace}` was empty, or workspace was explicitly set to $HOME
3. **Most likely vector:** Explore strand discovering beads but workspace resolution failing, falling back to default which was $HOME
4. **Systemic issue:** Two process leak incidents in 2 days suggests deeper problem

### Next Steps

1. ✅ Code review complete - identified all spawn points and workspace resolution logic
2. ⏳ Need historical logs from incident timeframe (may not exist if logs rotated)
3. ⏳ Need to check if Explore strand was active and what workspace_root was
4. ⏳ Review integration_tests contamination incident for patterns
5. ⏳ Test hypothesis: can we reproduce by setting workspace_root=$HOME and triggering dispatch?

## Files Reviewed

- `src/dispatch/mod.rs` - Agent dispatch, template rendering, process spawning
- `src/supervisor/mod.rs` - Worker spawning logic, supervisor loop
- `src/worker/mod.rs` - Workspace resolution, is_workspace_unset logic
- `src/config/mod.rs` - Default workspace resolution
- `src/strand/explore.rs` - Multi-workspace discovery
- Git history around 2026-07-28/29

## Related Incidents

- **2026-07-28:** Integration_tests process contamination (commit d76d71b)
  - 11.5 hour runtime, fake worker identity, 16 affected beads
  - Possible systemic issue with orphaned process handling

## Status

🔍 **Investigation ongoing** - Code review complete, evidence collection in progress
