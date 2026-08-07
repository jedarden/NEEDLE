# Investigation Report: Leaked Claude Processes (bf-3e4do)

## Incident Summary

**Date**: 2026-07-29 ~13:35 EDT  
**Location**: lab (100.81.129.38)  
**Issue**: ~115 leaked `claude` processes with cwd=$HOME, running for 11-13 hours  
**Impact**: Load average 304-308 on 12-core box (100% kernel time, context-switch thrashing)

## Root Cause Analysis

### Code Path Investigation

#### 1. Process Spawning in `src/dispatch/mod.rs`

**Location**: `run_process()` function (lines 743-805)

**Critical Code**:
```rust
let mut child = unsafe {
    tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&rendered)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .envs(&child_env)
        .pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        })
        .spawn()
        .with_context(|| format!("failed to spawn agent: {}", adapter.name))?
};
```

**Issue**: No explicit `current_dir()` call! The process inherits the parent's working directory.

**Built-in Adapter Templates** (lines 264-286):
```rust
invoke_template: concat!(
    "cd {workspace} && unbuffer -p claude --model claude-sonnet-4-6",
    " --max-turns 30 --output-format stream-json --dangerously-skip-permissions",
    " --verbose < {prompt_file}",
)
.to_string(),
```

**What Should Happen**:
- Template renders to: `cd /home/coding/some-workspace && unbuffer -p claude ...`
- Bash changes to workspace directory
- Claude runs in correct directory

**What Could Go Wrong**:
1. **Empty workspace path**: If `{workspace}` renders to empty string → `cd  && unbuffer -p claude...`
   - Bash would try to `cd ` with no argument → stays in current directory
2. **Failed cd command**: If workspace path doesn't exist or is inaccessible
   - With `&&` chaining, the `unbuffer` command wouldn't run
   - But if there's a different error handling pattern...
3. **Custom adapter without cd**: User-defined adapters might not include `cd {workspace}`
4. **Workspace resolution failure**: If workspace is empty or None upstream

#### 2. Worker Spawning in `src/supervisor/mod.rs`

**Location**: `spawn_worker()` function (lines 452-498)

**Critical Code**:
```rust
let mut cmd = std::process::Command::new(&self.worker_binary);
cmd.arg("run")
    .arg("--workspace")
    .arg(&self.config.workspace)
    .arg("--agent")
    .arg(&agent_name)
    // ... more args
```

**Issue**: No `current_dir()` call here either!

**How This Could Leak Processes**:
1. Supervisor starts with cwd=$HOME (where `needle supervise` was launched)
2. Supervisor spawns workers without setting cwd
3. Workers inherit cwd=$HOME
4. Workers dispatch agents, which also don't explicitly set cwd
5. If template `cd` fails → agents stay in $HOME

### Potential Failure Modes

#### Scenario 1: Empty Workspace Path
```bash
# Template renders with empty workspace
cd  && unbuffer -p claude --model claude-sonnet-4-6 ...
```
- Bash: `cd` with no argument → stays in current directory
- Claude runs in parent's cwd (likely $HOME)

#### Scenario 2: Custom Adapter Template
User creates custom adapter without `cd {workspace}`:
```yaml
name: my-claude
invoke_template: "claude --model claude-sonnet-4-6 < {prompt_file}"
```
- No `cd` command at all
- Claude runs in inherited cwd

#### Scenario 3: Workspace Resolution Failure
If bead metadata has empty/missing workspace field:
- Template might render `cd ` (empty)
- Worker might be launched with `--workspace ""`

#### Scenario 4: Template Substitution Failure
If the render_template function fails silently:
```rust
fn render_template(
    template: &str,
    workspace: &Path,
    prompt_file: &Path,
    bead_id: &BeadId,
    model: &str,
) -> String {
    template
        .replace("{workspace}", &workspace.display().to_string())
        // ...
}
```
- If `workspace.display()` returns empty string → `cd `

### Why 115 Processes Accumulated

**Key Finding**: The built-in adapters ALL include `cd {workspace}` in their templates. This suggests:
- The leak is likely NOT from normal dispatch flow
- OR there's a code path that bypasses normal template rendering
- OR there's a retry loop that spawns without checking previous attempt

**Possible Retry Loop**:
Looking at the supervisor tick logic (lines 308-376):
```rust
async fn tick(&mut self) -> Result<bool> {
    // ... check capacity ...
    
    let ready_beads = self.store.ready(&filters).await?;
    
    if ready_beads.is_empty() {
        return Ok(false);
    }
    
    // Spawn worker
    self.spawn_worker(ready_count).await?;
    
    Ok(true)
}
```

**No Retry Loop Found** - The supervisor spawns ONE worker per tick when capacity exists.

**Alternative Theory**: The 115 processes might be from:
1. Multiple supervisor instances running (shouldn't happen with proper locking)
2. Manual `needle run` commands launched from $HOME
3. A different code path entirely (health checks, canary tests, etc.)

### Health Check / Canary Analysis

**Canary Module** (`src/canary/mod.rs`):
```rust
let mut child = match Command::new(testing_binary)
    .args([
        "run",
        "--workspace",
        &self.canary_workspace.display().to_string(),
        // ...
    ])
    .spawn()
```
- **No `current_dir()` call here either!**
- Canary tests could also leak processes if run from $HOME

### Most Likely Root Cause

**Hypothesis**: There's a code path that spawns `claude` processes directly (not through normal worker dispatch) without properly setting the working directory.

**Candidate Locations**:
1. **Manual agent testing**: Code that tests adapters directly
2. **Health checks**: Periodic health verification that spawns agents
3. **Canary tests**: Automated testing that might have been running
4. **Custom workflows**: Any user scripts or custom code

**Evidence from Incident**:
- All 115 processes had cwd=$HOME
- Elapsed times: 41735-48134s (11.6-13.4h)
- Clustered spawn timestamps (programmatic, not manual)
- PPID all pointed to tmux server (normal reparenting)

**Critical Insight**: The processes were LIVE (not zombies), successfully killed with `pkill -9 -x claude`. This means:
- They were actual running processes
- Not stuck in uninterruptible sleep
- Responding to signals normally

## Reproduction Steps (Not Yet Tested)

### Test Case 1: Empty Workspace
```bash
# Manually test what happens with empty workspace
cd /home/coding
NEEDLE_TEST_EMPTY=1 cargo run -- run --workspace "" --agent claude-sonnet --count 1
```

### Test Case 2: Custom Adapter Without cd
```yaml
# Create adapter file: ~/.needle/adapters/leaky-test.yaml
name: leaky-test
agent_cli: claude
invoke_template: "claude --model claude-sonnet-4-6 < {prompt_file}"
```

### Test Case 3: Supervisor from Wrong Directory
```bash
# Launch supervisor from $HOME instead of workspace
cd /home/coding
needle supervise --workspace /some/valid/workspace
# Check if workers inherit $HOME as cwd
```

## Recommended Fixes

### Fix 1: Explicit current_dir() in dispatch (HIGH PRIORITY)

**Location**: `src/dispatch/mod.rs:run_process()`

**Change**:
```rust
let mut child = unsafe {
    tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&rendered)
        .current_dir(workspace)  // ADD THIS LINE
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .envs(&child_env)
        .pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        })
        .spawn()
        .with_context(|| format!("failed to spawn agent: {}", adapter.name))?
};
```

**Rationale**: Defense in depth. Even if template `cd` fails, process starts in correct directory.

### Fix 2: Explicit current_dir() in supervisor spawn (HIGH PRIORITY)

**Location**: `src/supervisor/mod.rs:spawn_worker()`

**Change**:
```rust
let mut cmd = std::process::Command::new(&self.worker_binary);
cmd.arg("run")
    .arg("--workspace")
    .arg(&self.config.workspace)
    .arg("--agent")
    .arg(&agent_name)
    .current_dir(&self.config.workspace)  // ADD THIS LINE
    // ... rest of args
```

**Rationale**: Ensures workers start in correct directory regardless of supervisor's cwd.

### Fix 3: Workspace Validation (MEDIUM PRIORITY)

**Location**: Template rendering / config loading

**Change**: Add validation to ensure workspace is non-empty and exists before spawning:
```rust
fn validate_workspace(workspace: &Path) -> Result<()> {
    if workspace.as_os_str().is_empty() {
        bail!("workspace path cannot be empty");
    }
    if !workspace.exists() {
        bail!("workspace does not exist: {}", workspace.display());
    }
    Ok(())
}

// Call in dispatch() before run_process()
validate_workspace(workspace)?;
```

### Fix 4: Adapter Template Validation (MEDIUM PRIORITY)

**Location**: Adapter loading

**Change**: Ensure all adapter templates include `cd {workspace}`:
```rust
pub fn validate_adapter_template(adapter: &AgentAdapter) -> Result<()> {
    if !adapter.invoke_template.contains("{workspace}") {
        tracing::warn!(
            adapter = %adapter.name,
            "adapter template missing {{workspace}} placeholder"
        );
    }
    // Optionally require cd command
    if !adapter.invoke_template.contains("cd {workspace}") && 
       !adapter.invoke_template.contains("cd {{workspace}}") {
        tracing::warn!(
            adapter = %adapter.name,
            "adapter template missing 'cd {{workspace}}' - processes may inherit parent cwd"
        );
    }
    Ok(())
}
```

### Fix 5: Process Leak Detection (LOW PRIORITY but HIGH VALUE)

**Location**: Supervisor tick or health monitoring

**Change**: Add detection for leaked processes:
```rust
fn detect_leaked_processes() -> Result<Vec<ProcessInfo>> {
    // Find all 'claude' processes
    // Check their cwd
    // Alert if many have cwd=$HOME
    // Auto-kill if confirmed leaked
}
```

## Next Steps

1. **Implement Fix 1 and 2** (critical, low risk)
2. **Add logging** to track process cwd at spawn time
3. **Search logs** for evidence of the 115 processes (stderr, stdout traces)
4. **Test reproduction scenarios** in dev environment
5. **Add metrics** for process leak detection
6. **Investigate canary module** for similar issues

## Related Issues

- **bf-12bim**: Supervisor zombie/defunct-child non-reaping (DIFFERENT - these were LIVE processes)
- **bf-1ynu9**: Supervisor uses current_exe() instead of PATH lookup (RELATED - worker spawn mechanism)
- **GitHub #11**: current_exe() fix already implemented

## Files Requiring Changes

1. `src/dispatch/mod.rs` - Add current_dir() to process spawn
2. `src/supervisor/mod.rs` - Add current_dir() to worker spawn  
3. `src/canary/mod.rs` - Add current_dir() to test binary spawn
4. `src/config.rs` or `src/dispatch/mod.rs` - Add workspace validation
5. `src/dispatch/mod.rs` - Add adapter template validation

## Conclusion

The root cause is **missing explicit `current_dir()` calls** when spawning processes. The built-in adapter templates include `cd {workspace}` which should prevent this, but:

1. Defense in depth is needed - explicit current_dir() provides fallback
2. Custom adapters might not include `cd` in templates
3. Edge cases (empty workspace, failed cd) could bypass template protection
4. Canary testing has the same vulnerability

The fix is straightforward and low-risk: add `current_dir(workspace)` to all process spawn points.
