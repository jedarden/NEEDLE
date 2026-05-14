# needle-r6vf: kill entire process group on agent timeout

## What was done

Fixed `src/dispatch/mod.rs` to kill the entire process group on agent timeout,
not just the direct bash child.

**Before:** `child.start_kill()` only sent SIGKILL to the direct child process.
Any subprocesses spawned by the agent (background jobs, subshells) became
orphans after timeout.

**After:**
1. `pre_exec(|| { libc::setpgid(0, 0); Ok(()) })` — places the agent bash
   process into its own process group immediately after fork, before exec.
   The group ID equals the child's PID.
2. On timeout: `libc::killpg(pid, libc::SIGKILL)` sends SIGKILL to the entire
   process group, reaping the agent and all its descendants atomically.
3. `child.start_kill()` is retained as a defensive fallback.

The `pid` captured from `child.id()` doubles as the process group ID because
`setpgid(0, 0)` sets the child's PID as its own group leader.
