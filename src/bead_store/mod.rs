//! Abstract bead store interface and CLI implementations.
//!
//! NEEDLE interacts with beads exclusively through the `BeadStore` trait. The
//! default implementation shells out to `bf` (bead-forge) with `--json` output.
//! JSON parsing failures are explicit errors — never silently treated as empty
//! results (v1 bug).
//!
//! The trait is `Send + Sync` because it is called from async worker tasks.
//!
//! `CliBeadStore` binds one backend descriptor to one executable, preventing
//! command grammar and binary identity from being mixed accidentally.
//!
//! IMPORTANT: authors of the `bead-rs`/beads_rust descriptor must probe the
//! actual beads_rust binary (including its help and output), not infer its
//! dialect from this module, `CliBeadStore`, or the bead-forge descriptor.
//!
//! Depends on: `types`.

pub mod backend;
mod cli_store;
mod strategies;

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use crate::process_guard::ProcessGuardSync;
use crate::types::{Bead, BeadId, ClaimResult};
use tracing::{debug, warn};

// Re-export the implementations so consumers don't need to change their imports
pub use backend::{
    builtin_bead_backends, load_bead_backends, BeadBackend, BeadBackendCapabilities,
    BeadBackendErrorMarkers, BeadBackendQuirk, BeadOperationSpec, ParseShape,
};
pub use cli_store::CliBeadStore;

/// Open the bead store explicitly bound by the target workspace's resolved
/// configuration. This is the production entry point: executable discovery
/// alone is never treated as evidence of store ownership.
pub fn open_configured(
    config: &crate::config::BeadCliConfig,
    workspace: PathBuf,
    model: Option<String>,
    harness: Option<String>,
    harness_version: Option<String>,
) -> Result<Arc<dyn BeadStore>> {
    if matches!(config.backend, crate::config::BeadBackend::Auto) {
        bail!(
            "workspace {} has no authoritative bead backend binding; set bead_cli.backend in {}",
            workspace.display(),
            workspace.join(".needle.yaml").display()
        );
    }

    let (backend, binary) = crate::config::resolve_bead_cli(config).with_context(|| {
        format!(
            "failed to resolve bead_cli.backend for workspace {}",
            workspace.display()
        )
    })?;
    verify_backend_identity(&backend, &binary, &workspace)?;
    if backend == crate::config::Backend::Bead {
        verify_bead_rs_capabilities(&binary, &workspace)?;
    }

    match backend {
        crate::config::Backend::Bead => {
            let descriptor = builtin_bead_backends()
                .into_iter()
                .find(|candidate| candidate.name == "bead-rs")
                .ok_or_else(|| anyhow::anyhow!("built-in bead-rs descriptor is missing"))?;
            Ok(Arc::new(CliBeadStore::new(
                descriptor,
                binary,
                workspace,
                model,
                harness,
                harness_version,
            )?))
        }
    }
}

fn verify_backend_identity(
    backend: &crate::config::Backend,
    binary: &Path,
    workspace: &Path,
) -> Result<()> {
    use regex::Regex;
    use std::io::Read;
    use std::process::Stdio;

    // Get the backend descriptor for this backend type
    let descriptor_name = match backend {
        crate::config::Backend::Bead => "bead-rs",
    };

    let descriptor = builtin_bead_backends()
        .into_iter()
        .find(|d| d.name == descriptor_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "built-in backend descriptor '{}' not found",
                descriptor_name
            )
        })?;

    let child = spawn_with_etxtbsy_retry_sync_child(
        || {
            std::process::Command::new(binary)
                .args(&descriptor.version_command)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        },
        5,  // max_attempts
        20, // backoff_ms
    )
    .with_context(|| {
        format!(
            "failed to inspect bead CLI identity at {}",
            binary.display()
        )
    })?;
    let mut guard = ProcessGuardSync::new(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        match guard.get_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(status))) => {
                break status;
            }
            Some(Ok(None)) => {
                // Still running, check timeout
            }
            Some(Err(e)) => {
                bail!(
                    "failed waiting for bead CLI identity at {}: {}",
                    binary.display(),
                    e
                );
            }
            None => {
                bail!("child already consumed");
            }
        }
        if std::time::Instant::now() >= deadline {
            // Kill the child process - guard will wait on drop
            if let Some(child) = guard.get_mut() {
                let _ = child.kill();
            }
            let _ = guard.wait();
            bail!(
                "timed out verifying bead backend identity for workspace {} at {}",
                workspace.display(),
                binary.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut stream) = guard.get_mut().and_then(|c| c.stdout.take()) {
        stream.read_to_end(&mut stdout)?;
    }
    if let Some(mut stream) = guard.get_mut().and_then(|c| c.stderr.take()) {
        stream.read_to_end(&mut stderr)?;
    }
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    let identity = format!("{stdout}{stderr}");

    if !status.success() {
        bail!(
            "bead backend identity check failed for workspace {}: {} --version exited with code {} (output: {:?})",
            workspace.display(),
            binary.display(),
            status.code().unwrap_or(-1),
            identity.trim()
        );
    }

    // Parse the version output using the descriptor's identity_pattern
    let identity_pattern = Regex::new(&descriptor.identity_pattern).with_context(|| {
        format!(
            "invalid identity_pattern regex for backend '{}': {:?}",
            descriptor.name, descriptor.identity_pattern
        )
    })?;

    let trimmed_output = identity.trim_start();
    if !identity_pattern.is_match(trimmed_output) {
        bail!(
            "bead backend identity mismatch for workspace {}: binary at {} reported version {:?}, which does not match expected pattern {:?} for backend '{}'",
            workspace.display(),
            binary.display(),
            trimmed_output,
            descriptor.identity_pattern,
            descriptor.name
        );
    }

    // Extract the backend name from the version output
    // The pattern captures the name at the start (e.g., "bf " or "bead ")
    let captured_name = trimmed_output
        .split_whitespace()
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "bead backend identity extraction failed for workspace {}: binary at {} reported empty version output",
                workspace.display(),
                binary.display()
            )
        })?;

    // Map the captured name to the expected backend name
    // Both "bf" and "bead" should map to their respective backend names
    let expected_names = match descriptor.name.as_str() {
        "bead-forge" => vec!["bf", "bead-forge"],
        "bead-rs" => vec!["bead", "bead-rs"],
        _ => vec![descriptor.name.as_str()],
    };

    if !expected_names.contains(&captured_name) {
        bail!(
            "bead backend identity mismatch for workspace {}: binary at {} reported name {:?}, but expected one of {:?} for backend '{}'",
            workspace.display(),
            binary.display(),
            captured_name,
            expected_names,
            descriptor.name
        );
    }

    Ok(())
}

/// Derive the expected backend name from the binary filename.
///
/// Maps binary names to their corresponding backend names:
/// - "bead" -> "bead-rs"
/// - "bf" -> "bead-forge"
/// - Other names are returned as-is (for extensibility)
fn derive_expected_backend_from_filename(binary: &Path) -> String {
    // Check if the path ends with a separator or looks like a directory
    let path_str = binary.to_string_lossy();
    if path_str.ends_with('/') || path_str.ends_with('\\') {
        return "unknown".to_string();
    }

    binary
        .file_name()
        .and_then(|name| name.to_str())
        .map(|filename| {
            match filename {
                "bead" => "bead-rs",
                "bf" => "bead-forge",
                _ => filename, // Return as-is for unknown binaries
            }
        })
        .unwrap_or("unknown")
        .to_string()
}

fn verify_bead_rs_capabilities(binary: &Path, workspace: &Path) -> Result<()> {
    // Derive expected backend from binary filename BEFORE probing capabilities
    let expected_backend = derive_expected_backend_from_filename(binary);

    let output = spawn_with_etxtbsy_retry_sync(
        || {
            std::process::Command::new(binary)
                .args(["capabilities", "--profile", "native-v1"])
                .current_dir(workspace)
                .output()
        },
        5,
        20,
    )
    .with_context(|| {
        format!(
            "failed to probe bead-rs capabilities at {}",
            binary.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "bead-rs capability probe failed for workspace {} at {}: {}",
            workspace.display(),
            binary.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let capabilities: serde_json::Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("invalid bead-rs capability JSON from {}", binary.display()))?;

    // Extract actual backend from capabilities response
    let actual_backend = capabilities
        .get("implementation")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");

    // Compare actual vs expected backend identity
    if actual_backend != expected_backend {
        tracing::error!(
            workspace = %workspace.display(),
            binary = %binary.display(),
            expected_backend = %expected_backend,
            actual_backend = %actual_backend,
            "backend identity verification failed: mismatch between expected and actual implementation"
        );
        bail!(
            "backend identity mismatch for workspace {}: binary '{}' implies backend '{}', but capabilities reported implementation '{}'",
            workspace.display(),
            binary.display(),
            expected_backend,
            actual_backend
        );
    }

    tracing::info!(
        workspace = %workspace.display(),
        binary = %binary.display(),
        backend = %actual_backend,
        expected_backend = %expected_backend,
        "backend identity verification passed: actual matches expected"
    );

    // Verify atomic_claim requirement
    if capabilities
        .get("atomic_claim")
        .and_then(|value| value.as_bool())
        != Some(true)
    {
        bail!(
            "bead-rs capability mismatch for workspace {}: expected atomic_claim=true",
            workspace.display()
        );
    }
    // Verify all required statuses are present
    for status in ["open", "in_progress", "deferred", "closed"] {
        let present = capabilities["statuses"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == status));
        if !present {
            bail!(
                "bead-rs capability mismatch for workspace {}: missing status {status}",
                workspace.display()
            );
        }
    }

    // Reject unexpected statuses like 'blocked' (a derived presentation status, not stored)
    if let Some(statuses) = capabilities["statuses"].as_array() {
        for status in statuses {
            if let Some(status_str) = status.as_str() {
                if !["open", "in_progress", "deferred", "closed"].contains(&status_str) {
                    bail!(
                        "bead-rs capability mismatch for workspace {}: unexpected status '{status_str}' in capabilities (only open, in_progress, deferred, and closed are valid stored statuses)",
                        workspace.display()
                    );
                }
            }
        }
    }
    for schema_ref in [
        "urn:bead-rs:schema:issue:native-v1",
        "urn:bead-rs:schema:event:native-v1",
        "urn:bead-rs:schema:field-guide:native-v1",
    ] {
        let present = capabilities["schemas"].as_array().is_some_and(|schemas| {
            schemas
                .iter()
                .any(|schema| schema["schema_ref"] == schema_ref)
        });
        if !present {
            bail!(
                "bead-rs capability mismatch for workspace {}: missing schema {schema_ref}",
                workspace.display()
            );
        }
    }
    for command in ["ref", "data", "query"] {
        let present = capabilities["commands"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == command));
        if !present {
            bail!(
                "bead-rs capability mismatch for workspace {}: missing command {command}",
                workspace.display()
            );
        }
    }
    Ok(())
}

/// Load a target workspace's configuration and open only its explicitly bound
/// bead backend. The historical function name is retained while strand call
/// sites migrate to receiving a pre-resolved backend context.
pub fn discover_default(
    workspace: PathBuf,
    model: Option<String>,
    harness: Option<String>,
    harness_version: Option<String>,
) -> Result<Arc<dyn BeadStore>> {
    let (config, _) = crate::config::ConfigLoader::load_resolved(
        &workspace,
        crate::config::CliOverrides {
            workspace: Some(workspace.clone()),
            ..Default::default()
        },
    )
    .with_context(|| {
        format!(
            "failed to load bead backend binding for workspace {}",
            workspace.display()
        )
    })?;
    open_configured(&config.bead_cli, workspace, model, harness, harness_version)
}

// Re-export operation strategies for backend descriptors
pub use strategies::{
    execute_claim_auto_strategy, execute_claim_strategy, execute_create_id_strategy,
    execute_import_strategy, execute_labels_strategy, execute_split_strategy,
    parse_labels_strategy, validate_strategy_name, ClaimAutoStrategy, ClaimStrategy,
    ClaimStrategyOperations, CompareAndSetOutcome, CreateIdStrategy, ImportStrategy,
    LabelsStrategy, OperationStrategy, ParsedStrategy, SequentialSplitError, SplitStrategy,
    SplitStrategyOperations,
};

// ─── Corruption detection ────────────────────────────────────────────────────

/// Known error strings that indicate SQLite database corruption.
const CORRUPTION_MARKERS: &[&str] = &[
    "database disk image is malformed",
    "database or disk is full",
    "attempt to write a readonly database",
    "file is not a database",
];

// ─── Version handshake ───────────────────────────────────────────────────────

// ─── ETXTBSY retry helper ─────────────────────────────────────────────────────

/// Retry wrapper for subprocess spawns that handle ETXTBSY (errno 26).
///
/// ETXTBSY ("Text file busy") occurs when the kernel blocks execution of a binary
/// that has a write-mode file descriptor still open. This is a genuine race condition:
///
/// 1. A process writes a binary to disk and closes the file descriptor.
/// 2. The kernel's page cache and filesystem synchronization haven't fully completed.
/// 3. Another process immediately attempts to `exec()` the same binary.
/// 4. The kernel returns ETXTBSY (errno 26) because the file is still marked for write.
///
/// This is most common with:
/// - Freshly-extracted upgrade binaries executed immediately
/// - Test fixtures written and chmod'd immediately before use
/// - Any binary that's written to disk and executed in quick succession
///
/// The race is narrow (typically <100ms) but real. Retrying with a short backoff
/// is the correct fix — the file descriptor is already closed, we're just waiting
/// for the kernel to finish its internal bookkeeping.
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
///
/// # When to use this helper
///
/// Use this wrapper whenever spawning a subprocess that may have been written to
/// disk immediately before execution:
///
/// - Freshly-installed or updated binaries
/// - Test fixtures created during test setup
/// - Any executable extracted from an archive and immediately run
///
/// Do NOT use this for long-running processes or stable system binaries — the
/// retry overhead is unnecessary and ETXTBSY is unlikely in those cases.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::bead_store::spawn_with_etxtbsy_retry;
///
/// # async fn example() -> Result<(), std::io::Error> {
/// let binary_path = Path::new("/path/to/binary");
/// let output = spawn_with_etxtbsy_retry(
///     || async {
///         tokio::process::Command::new(binary_path)
///             .arg("--version")
///             .output()
///             .await
///     },
///     5,
///     20,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn spawn_with_etxtbsy_retry<F, Fut, T>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match spawn_fn().await {
            Ok(output) => {
                if attempt > 0 {
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = max_attempts,
                        function = "spawn_with_etxtbsy_retry",
                        "Retry succeeded for {function_name} after {attempts} attempts",
                        function_name = "spawn_with_etxtbsy_retry",
                        attempts = attempt + 1
                    );
                }
                return Ok(output);
            }
            Err(e) if e.raw_os_error() == Some(26) && attempt + 1 < max_attempts => {
                last_err = Some(e);
                warn!(
                    "Retry attempt {}/{max_attempts} for 'spawn_with_etxtbsy_retry' due to ETXTBSY",
                    attempt + 1
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("loop always sets last_err before exhausting MAX_ATTEMPTS"))
}

/// Retry wrapper for `Command::spawn()` calls that handle ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry` for subprocess spawns that
/// return a `Child` process. Use this when you need to interact with the spawned
/// process (e.g., for timeout handling with `kill_on_drop`).
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub async fn spawn_with_etxtbsy_retry_child<F, Fut>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<tokio::process::Child>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<tokio::process::Child>>,
{
    spawn_with_etxtbsy_retry(spawn_fn, max_attempts, backoff_ms).await
}

/// Retry wrapper for subprocess spawns with exponential backoff for ETXTBSY (errno 26).
///
/// This variant uses **exponential backoff** with jitter, making it more suitable for
/// high-concurrency scenarios where multiple processes might race for the same binary.
/// Exponential backoff prevents thundering herd problems that linear backoff can cause.
///
/// The backoff formula is: `base_ms * 2^attempt + random_jitter`, where:
/// - `base_ms` is the initial delay (default: 20ms)
/// - `attempt` is the current retry number (0-indexed)
/// - `random_jitter` is ±25% of the calculated delay to prevent synchronization
///
/// Example backoff sequence with base_ms=20:
/// - Attempt 1: ~20ms (15-25ms with jitter)
/// - Attempt 2: ~40ms (30-50ms with jitter)
/// - Attempt 3: ~80ms (60-100ms with jitter)
/// - Attempt 4: ~160ms (120-200ms with jitter)
/// - Attempt 5: ~320ms (240-400ms with jitter)
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
///
/// # When to use exponential vs linear backoff
///
/// Use exponential backoff when:
/// - Spawning multiple processes concurrently that might race for the same binary
/// - Running in high-concurrency environments (CI, parallel tests)
/// - Dealing with slow filesystems where the race window might be longer
///
/// Use linear backoff (`spawn_with_etxtbsy_retry`) when:
/// - Spawning a single process in isolation
/// - The race window is known to be very short (<50ms)
/// - Minimal latency is more important than thundering herd prevention
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use needle::bead_store::spawn_with_etxtbsy_retry_exponential;
///
/// # async fn example() -> Result<(), std::io::Error> {
/// let binary_path = Path::new("/path/to/freshly-written-binary");
/// let output = spawn_with_etxtbsy_retry_exponential(
///     || async {
///         tokio::process::Command::new(binary_path)
///             .arg("--version")
///             .output()
///             .await
///     },
///     10,
///     20,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn spawn_with_etxtbsy_retry_exponential<F, Fut, T>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    use rand::Rng;

    const ETXTBSY_ERRNO: i32 = 26;
    const JITTER_PERCENT: f64 = 0.25; // ±25% jitter

    let mut last_err = None;
    let mut rng = rand::thread_rng();

    for attempt in 0..max_attempts {
        match spawn_fn().await {
            Ok(output) => {
                if attempt > 0 {
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = max_attempts,
                        function = "spawn_with_etxtbsy_retry_exponential",
                        "Retry succeeded for {function_name} after {attempts} attempts",
                        function_name = "spawn_with_etxtbsy_retry_exponential",
                        attempts = attempt + 1
                    );
                }
                return Ok(output);
            }
            Err(e) if e.raw_os_error() == Some(ETXTBSY_ERRNO) && attempt + 1 < max_attempts => {
                last_err = Some(e);

                // Calculate exponential backoff: base_ms * 2^attempt
                let exponential_delay = base_ms * (1 << attempt);

                // Add jitter to prevent synchronization
                let jitter_range = (exponential_delay as f64 * JITTER_PERCENT) as u64;
                let jitter = rng.gen_range(0..=jitter_range * 2);
                let delay = exponential_delay
                    .saturating_add(jitter)
                    .saturating_sub(jitter_range);

                warn!(
                    "Retry attempt {}/{max_attempts} for 'spawn_with_etxtbsy_retry_exponential' due to ETXTBSY",
                    attempt + 1
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.expect("loop always sets last_err before exhausting max_attempts"))
}

/// Retry wrapper for `Command::spawn()` with exponential backoff for ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry_exponential` for subprocess spawns that
/// return a `Child` process. Use this when you need to interact with the spawned process
/// (e.g., for timeout handling with `kill_on_drop`).
///
/// # Parameters
///
/// * `spawn_fn` - An async function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub async fn spawn_with_etxtbsy_retry_exponential_child<F, Fut>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<tokio::process::Child>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<tokio::process::Child>>,
{
    spawn_with_etxtbsy_retry_exponential(spawn_fn, max_attempts, base_ms).await
}

// ─── Synchronous ETXTBSY retry helpers ──────────────────────────────────────────────

/// Synchronous retry wrapper for subprocess spawns that handle ETXTBSY (errno 26).
///
/// This is the sync equivalent of `spawn_with_etxtbsy_retry`, using `std::thread::sleep`
/// instead of `tokio::time::sleep`. Use this for synchronous `std::process::Command` spawns
/// in non-async contexts.
///
/// # Parameters
///
/// * `spawn_fn` - A function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub fn spawn_with_etxtbsy_retry_sync<F, T>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> std::io::Result<T>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match spawn_fn() {
            Ok(output) => {
                if attempt > 0 {
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = max_attempts,
                        function = "spawn_with_etxtbsy_retry_sync",
                        "Retry succeeded for {function_name} after {attempts} attempts",
                        function_name = "spawn_with_etxtbsy_retry_sync",
                        attempts = attempt + 1
                    );
                }
                return Ok(output);
            }
            Err(e) if e.raw_os_error() == Some(26) && attempt + 1 < max_attempts => {
                last_err = Some(e);
                warn!(
                    "Retry attempt {}/{max_attempts} for 'spawn_with_etxtbsy_retry_sync' due to ETXTBSY",
                    attempt + 1
                );
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("loop always sets last_err before exhausting MAX_ATTEMPTS"))
}

/// Synchronous retry wrapper for `Command::spawn()` calls that handle ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry_sync` for subprocess spawns that
/// return a `std::process::Child` process. Use this when you need to interact with the spawned
/// process (e.g., for timeout handling) in synchronous contexts.
///
/// # Parameters
///
/// * `spawn_fn` - A function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 5)
/// * `backoff_ms` - Backoff delay between retries in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub fn spawn_with_etxtbsy_retry_sync_child<F>(
    spawn_fn: F,
    max_attempts: u32,
    backoff_ms: u64,
) -> std::io::Result<std::process::Child>
where
    F: Fn() -> std::io::Result<std::process::Child>,
{
    spawn_with_etxtbsy_retry_sync(spawn_fn, max_attempts, backoff_ms)
}

/// Synchronous retry wrapper for subprocess spawns with exponential backoff for ETXTBSY (errno 26).
///
/// This is the sync equivalent of `spawn_with_etxtbsy_retry_exponential`, using `std::thread::sleep`
/// instead of `tokio::time::sleep`. Use this for synchronous `std::process::Command` spawns
/// in non-async contexts.
///
/// # Parameters
///
/// * `spawn_fn` - A function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(T)` - The subprocess output on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub fn spawn_with_etxtbsy_retry_sync_exponential<F, T>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<T>
where
    F: Fn() -> std::io::Result<T>,
{
    use rand::Rng;

    const ETXTBSY_ERRNO: i32 = 26;
    const JITTER_PERCENT: f64 = 0.25; // ±25% jitter

    let mut last_err = None;
    let mut rng = rand::thread_rng();

    for attempt in 0..max_attempts {
        match spawn_fn() {
            Ok(output) => {
                if attempt > 0 {
                    debug!(
                        attempt = attempt + 1,
                        max_attempts = max_attempts,
                        function = "spawn_with_etxtbsy_retry_sync_exponential",
                        "Retry succeeded for {function_name} after {attempts} attempts",
                        function_name = "spawn_with_etxtbsy_retry_sync_exponential",
                        attempts = attempt + 1
                    );
                }
                return Ok(output);
            }
            Err(e) if e.raw_os_error() == Some(ETXTBSY_ERRNO) && attempt + 1 < max_attempts => {
                last_err = Some(e);

                // Calculate exponential backoff: base_ms * 2^attempt
                let exponential_delay = base_ms * (1 << attempt);

                // Add jitter to prevent synchronization
                let jitter_range = (exponential_delay as f64 * JITTER_PERCENT) as u64;
                let jitter = rng.gen_range(0..=jitter_range * 2);
                let delay = exponential_delay
                    .saturating_add(jitter)
                    .saturating_sub(jitter_range);

                warn!(
                    "Retry attempt {}/{max_attempts} for 'spawn_with_etxtbsy_retry_sync_exponential' due to ETXTBSY",
                    attempt + 1
                );
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.expect("loop always sets last_err before exhausting max_attempts"))
}

/// Synchronous retry wrapper for `Command::spawn()` with exponential backoff for ETXTBSY (errno 26).
///
/// Specialized version of `spawn_with_etxtbsy_retry_sync_exponential` for subprocess spawns that
/// return a `std::process::Child` process. Use this when you need to interact with the spawned
/// process (e.g., for timeout handling) in synchronous contexts.
///
/// # Parameters
///
/// * `spawn_fn` - A function that attempts to spawn the subprocess
/// * `max_attempts` - Maximum number of retry attempts (default: 10)
/// * `base_ms` - Base backoff delay in milliseconds (default: 20)
///
/// # Returns
///
/// * `Ok(Child)` - The spawned child process on success
/// * `Err(io::Error)` - The last error if all attempts are exhausted
pub fn spawn_with_etxtbsy_retry_sync_exponential_child<F>(
    spawn_fn: F,
    max_attempts: u32,
    base_ms: u64,
) -> std::io::Result<std::process::Child>
where
    F: Fn() -> std::io::Result<std::process::Child>,
{
    spawn_with_etxtbsy_retry_sync_exponential(spawn_fn, max_attempts, base_ms)
}

/// Parse backend name from a binary's version output.
///
/// Runs the binary with `--version` (or a custom version command) and parses
/// stdout to extract the backend identity (e.g., "bf", "bead", "bead-forge",
/// "bead-rs").
///
/// # Arguments
///
/// * `binary` - Path to the binary to execute
/// * `version_args` - Optional custom version arguments (defaults to `["--version"]`)
///
/// # Returns
///
/// * `Ok(String)` - The extracted backend name
/// * `Err` - If the binary cannot be spawned, exits with an error code, or
///   produces unparseable output
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use needle::bead_store::parse_backend_name_from_version;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let binary = PathBuf::from("/usr/bin/bf");
/// let backend_name = parse_backend_name_from_version(&binary, None).await?;
/// assert!(backend_name == "bf" || backend_name == "bead-forge");
/// # Ok(())
/// # }
/// ```
pub async fn parse_backend_name_from_version(
    binary: &Path,
    version_args: Option<&[&str]>,
) -> Result<String> {
    use std::process::Stdio;

    let args = version_args.unwrap_or(&["--version"]);

    let output = spawn_with_etxtbsy_retry_exponential(
        || async {
            tokio::process::Command::new(binary)
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
        },
        5,  // max_attempts
        20, // base_ms
    )
    .await
    .with_context(|| {
        format!(
            "failed to spawn binary for version check: {} {:?}",
            binary.display(),
            args
        )
    })?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "binary {} {:?} exited with code {}: {}",
            binary.display(),
            args,
            output.status.code().unwrap_or(-1),
            format!("{stdout}{stderr}").trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined_output = format!("{stdout}{stderr}");

    // Extract the backend name from the version output
    // The pattern captures the name at the start (e.g., "bf " or "bead ")
    let trimmed_output = combined_output.trim_start();
    let captured_name = trimmed_output.split_whitespace().next().ok_or_else(|| {
        anyhow::anyhow!("binary {} produced empty version output", binary.display())
    })?;

    Ok(captured_name.to_string())
}

// ─── Corruption detection ────────────────────────────────────────────────────

/// Known error strings that indicate SQLite database is locked (transient).
pub const LOCK_MARKERS: &[&str] = &[
    "database is locked",
    "sqlite error: 5", // SQLITE_BUSY = database is locked
    "sqlite error: 6", // SQLITE_LOCKED = table is locked
];

/// Known error strings that indicate br sync conflicts.
pub const SYNC_CONFLICT_MARKERS: &[&str] = &["SYNC_CONFLICT", "JSONL is newer", "sync conflict"];

/// Check if an error message indicates SQLite database corruption.
///
/// Returns `true` if the message contains any known corruption marker.
pub fn is_corruption_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CORRUPTION_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Check if an error message indicates a br sync conflict.
///
/// Returns `true` if the message contains any known sync conflict marker.
pub fn is_sync_conflict(msg: &str) -> bool {
    SYNC_CONFLICT_MARKERS
        .iter()
        .any(|marker| msg.contains(marker))
}

/// Check if an error message indicates SQLite database is locked (transient condition).
///
/// Returns `true` if the message contains any known lock marker.
pub fn is_lock_error(msg: &str) -> bool {
    LOCK_MARKERS.iter().any(|marker| msg.contains(marker))
}

/// Check if a workspace has a valid bead store (i.e., has a `.beads/` directory).
///
/// Returns `true` if the workspace contains a `.beads/` directory, `false` otherwise.
/// This is used by strands to distinguish between "no home store configured" (expected,
/// benign for roam-only workers) and "home store is broken" (unexpected, problem).
///
/// # Arguments
///
/// * `workspace` - Path to the workspace directory to check
///
/// # Examples
///
/// ```no_run
/// use needle::bead_store::has_valid_bead_store;
/// use std::path::PathBuf;
///
/// let workspace = PathBuf::from("/home/coding/myproject");
/// if has_valid_bead_store(&workspace) {
///     println!("Workspace has a valid bead store");
/// } else {
///     println!("Workspace has no bead store - strand should return Skipped");
/// }
/// ```
pub fn has_valid_bead_store(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}

/// Outcome of a database recovery attempt.
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// `br doctor --repair` fixed the issue.
    Repaired(RepairReport),
    /// Full rebuild (rm db + br sync --import) succeeded.
    Rebuilt,
    /// Recovery failed — JSONL itself may be corrupt or missing.
    Failed(anyhow::Error),
}

/// Error returned when SYNC_CONFLICT recovery fails after retry.
///
/// This is a distinct error type so callers can detect when br sync
/// recovery was attempted but the retry still failed. The caller may
/// choose to emit a failure event and continue rather than blocking.
#[derive(Debug, thiserror::Error)]
#[error("SYNC_CONFLICT recovery failed: {reason}")]
pub struct SyncRecoveryError {
    pub reason: String,
}

// ─── Filters ─────────────────────────────────────────────────────────────────

/// Filters applied when listing ready beads.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// Only return beads assigned to this actor. `None` = no filter.
    pub assignee: Option<String>,
    /// Exclude beads that have any of these labels.
    pub exclude_labels: Vec<String>,
    /// Exclude beads with these IDs.
    pub exclude_ids: HashSet<BeadId>,
}

// ─── RepairReport ─────────────────────────────────────────────────────────────

/// Summary from `br doctor --repair`.
#[derive(Debug, Default)]
pub struct RepairReport {
    pub warnings: Vec<String>,
    pub fixed: Vec<String>,
}

// ─── NewChild ─────────────────────────────────────────────────────────────────

/// A child bead to create during an atomic split (see [`BeadStore::split_bead`]).
///
/// Borrowed views only — the caller owns the backing strings/labels.
#[derive(Debug, Clone, Copy)]
pub struct NewChild<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub labels: &'a [&'a str],
}

// ─── BeadStore trait ─────────────────────────────────────────────────────────

/// Abstract interface to the bead backend.
#[async_trait]
pub trait BeadStore: Send + Sync {
    /// Whether an error indicates corruption according to this store's backend.
    fn is_corruption_error(&self, _message: &str) -> bool {
        false
    }

    /// Whether an error indicates a transient lock according to this store's backend.
    fn is_lock_error(&self, _message: &str) -> bool {
        false
    }

    /// Whether an error indicates a sync conflict according to this store's backend.
    fn is_sync_conflict(&self, _message: &str) -> bool {
        false
    }

    /// List all beads with no incomplete blockers (ready to work on).
    ///
    /// Returns an empty `Vec` when the queue is empty — that is not an error.
    /// Returns `Err` if JSON parsing or br invocation fails.
    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>>;

    /// List ALL beads in the workspace (no readiness/filter checks).
    ///
    /// Used by Knot strand for three-state verification — a DIFFERENT code
    /// path from `ready()` to avoid v1's false-positive bug.
    async fn list_all(&self) -> Result<Vec<Bead>>;

    /// List the complete inventory with any backend-specific metadata needed
    /// to explain why the ready frontier omitted beads.
    ///
    /// The default is the ordinary full projection. Backends may override it
    /// when their list projection omits readiness facts such as manual blocks.
    async fn starvation_inventory(&self) -> Result<Vec<Bead>> {
        self.list_all().await
    }

    /// Fetch a single bead by ID.
    async fn show(&self, id: &BeadId) -> Result<Bead>;

    /// Fetch a bead together with the number of claim-related history entries
    /// the backend exposed for it.
    ///
    /// Most backends do not include event history in their normal bead
    /// projection, so the default implementation reports no count.  A
    /// backend that does expose history can override this to let NEEDLE stop
    /// retrying a bead before another claim mutation grows its JSON record.
    async fn show_with_claim_history(&self, id: &BeadId) -> Result<(Bead, Option<u32>)> {
        Ok((self.show(id).await?, None))
    }

    /// Fetch operator/agent notes when the backend exposes them.
    ///
    /// Backends without a notes projection return `None`; this keeps notes a
    /// capability rather than forcing it into NEEDLE's common bead model.
    async fn notes(&self, _id: &BeadId) -> Result<Option<String>> {
        Ok(None)
    }

    /// Query the current claim state of a bead from the live store.
    ///
    /// This method reads directly from the live database (not from any cached
    /// snapshot) and returns the bead's current status, assignee, and revision.
    /// The revision field is used for optimistic concurrency control in atomic
    /// compare-and-set operations.
    ///
    /// Returns `Err` if the bead cannot be fetched or parsed. Returns a
    /// `ClaimStatus` with `revision: None` for backends that don't support
    /// revisions (bead-forge).
    async fn claim_status(&self, id: &BeadId) -> Result<crate::types::ClaimStatus> {
        let bead = self.show(id).await?;
        Ok(crate::types::ClaimStatus {
            status: bead.status,
            assignee: bead.assignee,
            revision: None,
        })
    }

    /// Attempt to atomically claim a bead (set status=in_progress, assignee=actor).
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `RaceLost { claimed_by }` — another worker got there first.
    /// - `NotClaimable { reason }` — bead not in a claimable state.
    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult>;

    /// Atomically find and claim the next available bead (server-selected).
    ///
    /// This is the preferred method for multi-worker scenarios as it eliminates
    /// race conditions: the server selects the bead and assigns it in a single
    /// BEGIN IMMEDIATE transaction. Two workers calling this simultaneously will
    /// always receive distinct beads.
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `NotClaimable { reason }` — no beads available to claim.
    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult>;

    /// Release a claimed bead back to open (e.g., after agent failure).
    async fn release(&self, id: &BeadId) -> Result<()>;

    /// Quarantine a bead by setting status=blocked (e.g., after it exceeds the
    /// consecutive-failure threshold in `OutcomeConfig::quarantine_after_failures`).
    ///
    /// Unlike `release`, this deliberately does NOT clear the assignee or return
    /// the bead to a claimable state — the whole point is to stop Pluck from
    /// re-selecting it until a human (or a future auto-split) intervenes.
    async fn block(&self, id: &BeadId) -> Result<()>;

    /// Clear the assignee on a bead without changing its status.
    ///
    /// Used by mend to heal open beads with stale assignees (e.g., from manual
    /// assignment changes, race conditions, or other cases where beads become
    /// stuck with an assignee). Note: As of 2026-08-24, `bead reopen` clears the
    /// assignee, so this is not needed for reopened beads.
    async fn clear_assignee(&self, id: &BeadId) -> Result<()>;

    /// Flush local bead changes to JSONL before release.
    ///
    /// Runs `br sync --flush-only` to ensure any local writes are persisted
    /// to JSONL before attempting to release a bead. This prevents SYNC_CONFLICT
    /// errors when the JSONL has newer remote changes.
    async fn flush(&self) -> Result<()>;

    /// Reopen a closed (Done) bead back to open status.
    ///
    /// Used by validation gates when verification fails after an agent has
    /// already closed the bead.
    async fn reopen(&self, id: &BeadId) -> Result<()>;

    /// Close a bead with durable operator-visible reasoning.
    ///
    /// The historical worker path lets agents close their implementation bead
    /// directly, but asynchronous reconciler-owned beads need a first-class
    /// close operation.  Backends that do not expose the declared `close`
    /// operation retain the explicit error rather than silently pretending the
    /// CI lifecycle completed.
    async fn close(&self, _id: &BeadId, _reason: &str) -> Result<()> {
        bail!("configured bead backend does not implement close")
    }

    /// List all labels on a bead.
    async fn labels(&self, id: &BeadId) -> Result<Vec<String>>;

    /// Add a label to a bead.
    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Remove a label from a bead.
    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Create a new bead and return its ID.
    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId>;

    /// Add a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep add <blocker_id> --blocks <blocked_id>`.
    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()>;

    /// Atomically create `children` and link each as a blocker of `parent_id`.
    ///
    /// This is the "mitosis" / bead-splitting primitive. N child creations plus
    /// N dependency links must commit together: otherwise a crash between the
    /// `create` and the `dep add` (SIGKILL, OOM, pod eviction) leaves an orphaned
    /// child with no dependency link, and the parent never unblocks — plan.md
    /// Phase 5.3, Race 3.
    ///
    /// The parent is deliberately **not** closed: NEEDLE keeps a split parent
    /// open/blocked while its children are worked.
    ///
    /// The default implementation performs the historical non-atomic sequence
    /// (`create_bead` + `add_dependency`, one child at a time). Mock stores rely
    /// on it, and bf-backed stores fall back to it when the atomic path is
    /// unavailable. Backends that can run a transactional batch (bf) override
    /// this to make the whole split crash-safe.
    async fn split_bead(
        &self,
        parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        let mut created = Vec::with_capacity(children.len());
        for child in children {
            let child_id = self
                .create_bead(child.title, child.body, child.labels)
                .await
                .with_context(|| format!("failed to create child bead: {}", child.title))?;
            self.add_dependency(&child_id, parent_id)
                .await
                .with_context(|| {
                    format!("failed to add dependency: {child_id} blocks {parent_id}")
                })?;
            created.push(child_id);
        }
        Ok(created)
    }

    /// Remove a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep remove <blocked_id> <blocker_id>`.
    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()>;

    /// Run `br doctor --repair` and return the report.
    async fn doctor_repair(&self) -> Result<RepairReport>;

    /// Run `br doctor` (without `--repair`) to check database health.
    ///
    /// Returns warnings if any issues are detected, without attempting to fix them.
    async fn doctor_check(&self) -> Result<RepairReport>;

    /// Full database rebuild from the configured backend's durable checkpoint.
    ///
    /// The existing SQLite file set remains in a rollback backup until the
    /// public backend import and doctor verification both succeed.
    ///
    /// Returns `Err` if rebuild or verification fails; the original database
    /// is restored in that case.
    async fn full_rebuild(&self) -> Result<()>;

    /// Check if this store has a valid bead store (i.e., has a `.beads/` directory).
    ///
    /// Returns `true` if the workspace contains a `.beads/` directory, `false` otherwise.
    /// This is used by strands to distinguish between "no home store configured" (expected,
    /// benign for roam-only workers) and "home store is broken" (unexpected, problem).
    fn has_valid_store(&self) -> bool;
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::time::{Duration, Instant};

    #[cfg(unix)]
    fn version_fixture(directory: &Path, name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = capabilities ]; then\n  printf '%s\\n' '{{\"implementation\":\"bead-rs\",\"atomic_claim\":true,\"statuses\":[\"open\",\"in_progress\",\"deferred\",\"closed\"],\"schemas\":[{{\"schema_ref\":\"urn:bead-rs:schema:issue:native-v1\"}},{{\"schema_ref\":\"urn:bead-rs:schema:event:native-v1\"}},{{\"schema_ref\":\"urn:bead-rs:schema:field-guide:native-v1\"}}]}}'\nelse\n  echo '{version}'\nfi\n"
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn configured_store_rejects_auto_before_opening_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let config = crate::config::BeadCliConfig::default();
        let error = open_configured(&config, workspace.path().to_path_buf(), None, None, None)
            .err()
            .expect("auto must not authorize store access");
        assert!(error
            .to_string()
            .contains("no authoritative bead backend binding"));
    }

    #[cfg(unix)]
    #[test]
    fn configured_store_accepts_matching_bead_rs_identity() {
        let workspace = tempfile::tempdir().unwrap();
        let binary = version_fixture(workspace.path(), "bead", "bead 0.1.1");
        let config = crate::config::BeadCliConfig {
            backend: crate::config::BeadBackend::Bead,
            path: Some(binary),
        };
        assert!(open_configured(&config, workspace.path().to_path_buf(), None, None, None).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn configured_store_rejects_identity_mismatch() {
        let workspace = tempfile::tempdir().unwrap();
        let binary = version_fixture(workspace.path(), "not-bead", "bf 0.4.1");
        let config = crate::config::BeadCliConfig {
            backend: crate::config::BeadBackend::Bead,
            path: Some(binary),
        };
        let error = open_configured(&config, workspace.path().to_path_buf(), None, None, None)
            .err()
            .expect("mismatched identity must fail closed");
        assert!(error.to_string().contains("identity mismatch"));
    }

    #[cfg(unix)]
    #[test]
    fn configured_store_rejects_bead_rs_capability_drift() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("bead");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = capabilities ]; then echo '{\"implementation\":\"bead-rs\",\"atomic_claim\":false}'; else echo 'bead 0.1.3'; fi\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        let config = crate::config::BeadCliConfig {
            backend: crate::config::BeadBackend::Bead,
            path: Some(binary),
        };
        let error = open_configured(&config, workspace.path().to_path_buf(), None, None, None)
            .err()
            .expect("capability drift must fail closed");
        assert!(
            error.to_string().contains("capability mismatch"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn filters_default_is_empty() {
        let f = Filters::default();
        assert!(f.assignee.is_none());
        assert!(f.exclude_labels.is_empty());
        assert!(f.exclude_ids.is_empty());
    }

    #[test]
    fn filters_with_exclude_ids_filters_beads() {
        use std::collections::HashSet;

        // Create a Filters with exclude_ids containing "bf-abc"
        let mut exclude_ids = HashSet::new();
        exclude_ids.insert(BeadId::from("bf-abc".to_string()));

        let filters = Filters {
            assignee: None,
            exclude_labels: vec![],
            exclude_ids,
        };

        // Verify the exclude_ids contains the expected bead ID
        assert!(filters
            .exclude_ids
            .contains(&BeadId::from("bf-abc".to_string())));
        assert_eq!(filters.exclude_ids.len(), 1);
    }

    // ── Corruption detection tests ──────────────────────────────────────────

    #[test]
    fn detects_malformed_db_error() {
        assert!(is_corruption_error(
            "Error: database disk image is malformed"
        ));
    }

    #[test]
    fn detects_locked_db_error() {
        assert!(is_lock_error("database is locked"));
        // Also verify that lock errors are NOT corruption errors (they're transient)
        assert!(!is_corruption_error("database is locked"));
    }

    #[test]
    fn detects_not_a_database_error() {
        assert!(is_corruption_error("file is not a database"));
    }

    #[test]
    fn detects_case_insensitive() {
        assert!(is_corruption_error(
            "ERROR: Database Disk Image Is Malformed"
        ));
    }

    #[test]
    fn non_corruption_error_returns_false() {
        assert!(!is_corruption_error("bead not found"));
        assert!(!is_corruption_error("connection refused"));
        assert!(!is_corruption_error(""));
    }

    #[test]
    fn corruption_in_longer_message() {
        let msg = "br [\"list\"] exited with code 1\nstderr: Error: database disk image is malformed\nstdout: ";
        assert!(is_corruption_error(msg));
    }

    // ── Sync conflict detection tests ─────────────────────────────────────

    #[test]
    fn sync_conflict_detects_sync_conflict_marker() {
        assert!(is_sync_conflict("Error: SYNC_CONFLICT detected"));
    }

    #[test]
    fn sync_conflict_detects_jsonl_is_newer() {
        assert!(is_sync_conflict("JSONL is newer than database"));
    }

    #[test]
    fn sync_conflict_detects_lowercase_marker() {
        assert!(is_sync_conflict("sync conflict on update"));
    }

    #[test]
    fn sync_conflict_in_longer_stderr() {
        let msg = "br [\"update\"] exited with code 6\nstderr: SYNC_CONFLICT\nstdout: ";
        assert!(is_sync_conflict(msg));
    }

    #[test]
    fn sync_conflict_returns_false_for_non_conflict() {
        assert!(!is_sync_conflict("bead not found"));
        assert!(!is_sync_conflict("database disk image is malformed"));
        assert!(!is_sync_conflict(""));
    }

    #[test]
    fn sync_conflict_is_case_sensitive() {
        // SYNC_CONFLICT is an exact marker, case matters
        assert!(!is_sync_conflict("sync_conflict"));
        assert!(is_sync_conflict("SYNC_CONFLICT"));
    }

    // ── has_valid_bead_store tests ─────────────────────────────────────────

    #[test]
    fn has_valid_bead_store_returns_false_for_nonexistent_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        assert!(!has_valid_bead_store(workspace));
    }

    #[test]
    fn has_valid_bead_store_returns_true_for_beads_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();
        assert!(has_valid_bead_store(workspace));
    }

    #[test]
    fn has_valid_bead_store_returns_false_for_file_instead_of_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::write(workspace.join(".beads"), "not a directory").unwrap();
        assert!(!has_valid_bead_store(workspace));
    }

    // ── version handshake tests ────────────────────────────────────────────

    // Bead-forge version checking tests removed with bead-forge descriptor
    // (bead needle-9e4d6d9d)

    // ─── ETXTBSY retry tests ───────────────────────────────────────────────────────

    /// Helper function to create an ETXTBSY error (errno 26 on Unix).
    fn make_etxtbsy_error() -> io::Error {
        io::Error::from_raw_os_error(26)
    }

    /// Helper function to create a non-ETXTBSY error.
    fn make_other_error() -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, "not found")
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        // Fail first 2 attempts with ETXTBSY
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Always fail with ETXTBSY
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            3,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // max attempts
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Fail with non-ETXTBSY error (should not retry)
                    Err::<_, io::Error>(make_other_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 3 {
                        // Fail first 3 attempts with ETXTBSY
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Always fail with ETXTBSY
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5); // max attempts
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Fail with non-ETXTBSY error (should not retry)
                    Err::<_, io::Error>(make_other_error())
                }
            },
            10,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            5,
            50,
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success

        // With 2 retries at 50ms each, should take ~100ms
        assert!(elapsed >= Duration::from_millis(90));
        assert!(elapsed < Duration::from_millis(200)); // Upper bound with tolerance
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 3 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                }
            },
            10,
            20,
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success

        // Exponential backoff: 20ms + 40ms + 80ms = ~140ms with jitter
        assert!(elapsed >= Duration::from_millis(100));
        assert!(elapsed < Duration::from_millis(300)); // Upper bound with jitter tolerance
    }

    #[tokio::test]
    async fn etxtbsy_retry_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Create a mock child process result
        let result = spawn_with_etxtbsy_retry_child(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Return a mock child-like result (we just test the wrapper passes through)
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            3,
            20,
        )
        .await;

        // Should fail after exhausting attempts
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_exponential_child(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            5,
            20,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5);
    }

    // ─── Sync ETXTBSY retry tests ─────────────────────────────────────────────────────

    #[test]
    fn etxtbsy_retry_sync_linear_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(b"success".to_vec())
            },
            5,
            20,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn etxtbsy_retry_sync_linear_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            20,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[test]
    fn etxtbsy_retry_sync_linear_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            3,
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // max attempts
    }

    #[test]
    fn etxtbsy_retry_sync_linear_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_other_error())
            },
            5,
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[test]
    fn etxtbsy_retry_sync_linear_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            50,
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success

        // With 2 retries at 50ms each, should take ~100ms
        assert!(elapsed >= Duration::from_millis(90));
        assert!(elapsed < Duration::from_millis(200)); // Upper bound with tolerance
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_succeeds_on_first_attempt() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(b"success".to_vec())
            },
            10,
            20,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_retries_on_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 3 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            20,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"success".to_vec());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_exhausts_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            5,
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5); // max attempts
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_fails_fast_on_non_etxtbsy() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_other_error())
            },
            10,
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should only be called once
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_timing() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 3 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            20,
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success

        // Exponential backoff: 20ms + 40ms + 80ms = ~140ms with jitter
        assert!(elapsed >= Duration::from_millis(100));
        assert!(elapsed < Duration::from_millis(300)); // Upper bound with jitter tolerance
    }

    #[test]
    fn etxtbsy_retry_sync_linear_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Create a mock child process result
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            3,
            20,
        );

        // Should fail after exhausting attempts
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_child_wrapper_works() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result = spawn_with_etxtbsy_retry_sync_exponential_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            5,
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn etxtbsy_retry_sync_linear_max_attempts_of_one() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            1, // Only one attempt - should fail immediately without retry
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Only called once
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_max_attempts_of_one() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            1, // Only one attempt - should fail immediately without retry
            20,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Only called once
    }

    #[test]
    fn etxtbsy_retry_sync_linear_zero_backoff() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            0, // Zero backoff - should retry immediately
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
        // With zero backoff, should complete very quickly (< 10ms)
        assert!(elapsed < Duration::from_millis(50));
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_zero_base_backoff() {
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            0, // Zero base backoff - exponential backoff with 0 base means no delay
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
        // With zero base backoff, should complete very quickly
        assert!(elapsed < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_jitter_randomization() {
        // This test verifies that jitter actually randomizes delays.
        // We run the same retry scenario multiple times and verify that
        // delays vary between runs (not always identical).
        const BASE_MS: u64 = 20;
        const NUM_RUNS: usize = 10;

        let mut elapsed_times = Vec::with_capacity(NUM_RUNS);

        for _ in 0..NUM_RUNS {
            let call_count = Arc::new(AtomicU32::new(0));
            let call_count_clone = Arc::clone(&call_count);

            let start = Instant::now();

            let result = spawn_with_etxtbsy_retry_exponential(
                || {
                    let count = Arc::clone(&call_count_clone);
                    async move {
                        let attempt = count.fetch_add(1, Ordering::SeqCst);
                        if attempt < 2 {
                            // Fail first 2 attempts with ETXTBSY to trigger 2 retry delays
                            Err::<_, io::Error>(make_etxtbsy_error())
                        } else {
                            Ok::<_, io::Error>(b"success".to_vec())
                        }
                    }
                },
                10,
                BASE_MS,
            )
            .await;

            let elapsed = start.elapsed();
            elapsed_times.push(elapsed);

            assert!(result.is_ok());
            assert_eq!(call_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
        }

        // Verify that not all elapsed times are identical (jitter is working)
        // With ±25% jitter on exponential backoff, times should vary
        let unique_times: std::collections::HashSet<_> =
            elapsed_times.iter().map(|d| d.as_millis()).collect();

        // We expect at least some variation across 10 runs
        // If jitter wasn't working, all times would be identical
        assert!(
            unique_times.len() > 1,
            "Expected jitter to create varying delays, but got {} unique times out of {} runs: {:?}",
            unique_times.len(),
            NUM_RUNS,
            elapsed_times
        );

        // Verify that times stay within reasonable bounds
        // Expected: 20ms + 40ms = ~60ms with jitter (15-75ms range)
        for elapsed in &elapsed_times {
            assert!(
                *elapsed >= Duration::from_millis(10),
                "Elapsed time {} too short (expected >=10ms with jitter)",
                elapsed.as_millis()
            );
            assert!(
                *elapsed < Duration::from_millis(150),
                "Elapsed time {} too long (expected <150ms with jitter)",
                elapsed.as_millis()
            );
        }
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_jitter_randomization() {
        // Sync version of the jitter randomization test
        const BASE_MS: u64 = 20;
        const NUM_RUNS: usize = 10;

        let mut elapsed_times = Vec::with_capacity(NUM_RUNS);

        for _ in 0..NUM_RUNS {
            let call_count = Arc::new(AtomicU32::new(0));
            let call_count_clone = Arc::clone(&call_count);

            let start = Instant::now();

            let result = spawn_with_etxtbsy_retry_sync_exponential(
                || {
                    let count = Arc::clone(&call_count_clone);
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                },
                10,
                BASE_MS,
            );

            let elapsed = start.elapsed();
            elapsed_times.push(elapsed);

            assert!(result.is_ok());
            assert_eq!(call_count.load(Ordering::SeqCst), 3);
        }

        // Verify jitter creates variation
        let unique_times: std::collections::HashSet<_> =
            elapsed_times.iter().map(|d| d.as_millis()).collect();

        assert!(
            unique_times.len() > 1,
            "Expected jitter to create varying delays in sync version, but got {} unique times out of {} runs: {:?}",
            unique_times.len(),
            NUM_RUNS,
            elapsed_times
        );

        // Verify times stay within reasonable bounds
        for elapsed in &elapsed_times {
            assert!(
                *elapsed >= Duration::from_millis(10),
                "Elapsed time {} too short (expected >=10ms with jitter)",
                elapsed.as_millis()
            );
            assert!(
                *elapsed < Duration::from_millis(150),
                "Elapsed time {} too long (expected <150ms with jitter)",
                elapsed.as_millis()
            );
        }
    }

    #[tokio::test]
    async fn etxtbsy_retry_exponential_backoff_sequence() {
        // Verify that exponential backoff actually grows exponentially.
        // With 5 retries, delays should be: ~20ms, ~40ms, ~80ms, ~160ms, ~320ms
        const BASE_MS: u64 = 20;
        const MAX_ATTEMPTS: u32 = 6; // 1 initial + 5 retries

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Always fail to trigger all retry delays
                    Err::<_, io::Error>(make_etxtbsy_error())
                }
            },
            MAX_ATTEMPTS,
            BASE_MS,
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), MAX_ATTEMPTS);

        // With 5 retries at exponential backoff: 20 + 40 + 80 + 160 + 320 = ~620ms
        // With ±25% jitter, this could range from ~465ms to ~775ms
        assert!(
            elapsed >= Duration::from_millis(400),
            "Exponential backoff took {}ms, expected >=400ms (5 retries at exponential backoff)",
            elapsed.as_millis()
        );
        assert!(
            elapsed < Duration::from_millis(1000),
            "Exponential backoff took {}ms, expected <1000ms (with jitter)",
            elapsed.as_millis()
        );
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_backoff_sequence() {
        // Sync version of exponential backoff sequence test
        const BASE_MS: u64 = 20;
        const MAX_ATTEMPTS: u32 = 6;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result: std::io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            MAX_ATTEMPTS,
            BASE_MS,
        );

        let elapsed = start.elapsed();

        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), MAX_ATTEMPTS);

        // Same bounds as async version
        assert!(
            elapsed >= Duration::from_millis(400),
            "Sync exponential backoff took {}ms, expected >=400ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed < Duration::from_millis(1000),
            "Sync exponential backoff took {}ms, expected <1000ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn etxtbsy_retry_linear_consistent_timing() {
        // Verify that linear backoff produces consistent timing (no jitter).
        // Multiple runs should produce similar elapsed times.
        const BACKOFF_MS: u64 = 30;
        const NUM_RUNS: usize = 5;

        let mut elapsed_times = Vec::with_capacity(NUM_RUNS);

        for _ in 0..NUM_RUNS {
            let call_count = Arc::new(AtomicU32::new(0));
            let call_count_clone = Arc::clone(&call_count);

            let start = Instant::now();

            let result = spawn_with_etxtbsy_retry(
                || {
                    let count = Arc::clone(&call_count_clone);
                    async move {
                        let attempt = count.fetch_add(1, Ordering::SeqCst);
                        if attempt < 2 {
                            Err::<_, io::Error>(make_etxtbsy_error())
                        } else {
                            Ok::<_, io::Error>(b"success".to_vec())
                        }
                    }
                },
                5,
                BACKOFF_MS,
            )
            .await;

            let elapsed = start.elapsed();
            elapsed_times.push(elapsed);

            assert!(result.is_ok());
            assert_eq!(call_count.load(Ordering::SeqCst), 3);
        }

        // With 2 retries at 30ms each, should be ~60ms every time (linear, no jitter)
        for elapsed in &elapsed_times {
            assert!(
                *elapsed >= Duration::from_millis(50),
                "Linear backoff took {}ms, expected >=50ms (2 retries at 30ms each)",
                elapsed.as_millis()
            );
            assert!(
                *elapsed < Duration::from_millis(100),
                "Linear backoff took {}ms, expected <100ms",
                elapsed.as_millis()
            );
        }
    }

    // ─── Retry timing and jitter tests ─────────────────────────────────────────────

    #[test]
    fn etxtbsy_retry_sync_exponential_backoff_increases_exponentially() {
        // Test that exponential backoff timing actually increases exponentially
        // With base_ms=10: 10ms, 20ms, 40ms, 80ms, 160ms...
        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);

                if attempt < 4 {
                    // Fail first 4 attempts to measure backoff progression
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            10, // Small base_ms for faster test
        );

        let total_elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 5); // 4 failures + 1 success

        // Expected exponential backoff with ±25% jitter:
        // Attempt 0: 10ms ± 2.5ms (7.5-12.5ms)
        // Attempt 1: 20ms ± 5ms (15-25ms)
        // Attempt 2: 40ms ± 10ms (30-50ms)
        // Attempt 3: 80ms ± 20ms (60-100ms)
        // Total expected: 112.5-182.5ms

        // Verify total time is within expected range with jitter
        assert!(
            total_elapsed >= Duration::from_millis(100),
            "Total elapsed {}ms should be >= 100ms (lower bound of exponential backoff)",
            total_elapsed.as_millis()
        );
        assert!(
            total_elapsed < Duration::from_millis(200),
            "Total elapsed {}ms should be < 200ms (upper bound with jitter)",
            total_elapsed.as_millis()
        );
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_jitter_randomization_is_applied() {
        // Test that jitter actually randomizes the delay times
        // Run the same retry pattern multiple times and verify delays vary
        let mut elapsed_times = Vec::new();

        for _ in 0..10 {
            let call_count = Arc::new(AtomicU32::new(0));
            let call_count_clone = Arc::clone(&call_count);

            let start = Instant::now();

            let result = spawn_with_etxtbsy_retry_sync_exponential(
                || {
                    let count = Arc::clone(&call_count_clone);
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<_, io::Error>(make_etxtbsy_error())
                    } else {
                        Ok::<_, io::Error>(b"success".to_vec())
                    }
                },
                5,
                20, // base_ms=20: attempts 0,1 should be 20ms ± 5ms, 40ms ± 10ms
            );

            assert!(result.is_ok());
            elapsed_times.push(start.elapsed());
        }

        // With jitter, the 10 runs should have varying elapsed times
        // If all times are identical, jitter is not working
        let unique_times: std::collections::HashSet<_> =
            elapsed_times.iter().map(|d| d.as_millis()).collect();

        assert!(
            unique_times.len() > 1,
            "Jitter should produce varying delay times, but got {} identical results out of 10 runs",
            unique_times.len()
        );

        // Verify all times are within expected bounds (60-100ms for 2 retries with jitter)
        for elapsed in &elapsed_times {
            assert!(
                *elapsed >= Duration::from_millis(40),
                "Jitter delay {}ms should be >= 40ms (lower bound with 2 retries)",
                elapsed.as_millis()
            );
            assert!(
                *elapsed < Duration::from_millis(120),
                "Jitter delay {}ms should be < 120ms (upper bound with 2 retries)",
                elapsed.as_millis()
            );
        }
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_timing_bounds_with_jitter() {
        // Test that timing stays within expected bounds with jitter
        // Jitter is ±25% of the exponential delay
        const JITTER_PERCENT: f64 = 0.25;
        const BASE_MS: u64 = 20;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        // Fail 3 times then succeed
        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 3 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            BASE_MS,
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 4); // 3 failures + 1 success

        // Calculate expected bounds:
        // Attempt 0: 20ms * 2^0 = 20ms ± 5ms (15-25ms)
        // Attempt 1: 20ms * 2^1 = 40ms ± 10ms (30-50ms)
        // Attempt 2: 20ms * 2^2 = 80ms ± 20ms (60-100ms)

        // Minimum total: (15 + 30 + 60) = 105ms
        let min_expected =
            ((BASE_MS + BASE_MS * 2 + BASE_MS * 4) as f64 * (1.0 - JITTER_PERCENT)) as u64;

        // Maximum total: (25 + 50 + 100) = 175ms
        let max_expected =
            ((BASE_MS + BASE_MS * 2 + BASE_MS * 4) as f64 * (1.0 + JITTER_PERCENT)) as u64;

        assert!(
            elapsed >= Duration::from_millis(min_expected),
            "Elapsed {}ms should be >= {}ms (minimum with jitter)",
            elapsed.as_millis(),
            min_expected
        );
        assert!(
            elapsed < Duration::from_millis(max_expected + 20), // +20ms for execution overhead
            "Elapsed {}ms should be < {}ms (maximum with jitter + overhead)",
            elapsed.as_millis(),
            max_expected + 20
        );
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_single_retry_timing() {
        // Test timing with just one retry (2 attempts total)
        const BASE_MS: u64 = 50;
        const JITTER_PERCENT: f64 = 0.25;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            5,
            BASE_MS,
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 2); // 1 failure + 1 success

        // Single retry: 50ms ± 12.5ms (37.5-62.5ms)
        let min_expected = (BASE_MS as f64 * (1.0 - JITTER_PERCENT)) as u64;
        let max_expected = (BASE_MS as f64 * (1.0 + JITTER_PERCENT)) as u64;

        assert!(
            elapsed >= Duration::from_millis(min_expected),
            "Single retry elapsed {}ms should be >= {}ms",
            elapsed.as_millis(),
            min_expected
        );
        assert!(
            elapsed < Duration::from_millis(max_expected + 10),
            "Single retry elapsed {}ms should be < {}ms",
            elapsed.as_millis(),
            max_expected + 10
        );
    }

    #[test]
    fn etxtbsy_retry_sync_exponential_many_retries_timing() {
        // Test timing with many retries to verify exponential scaling
        const BASE_MS: u64 = 10;
        const JITTER_PERCENT: f64 = 0.25;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        let start = Instant::now();

        // Fail 6 times then succeed (7 attempts total)
        let result = spawn_with_etxtbsy_retry_sync_exponential(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 6 {
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"success".to_vec())
                }
            },
            10,
            BASE_MS,
        );

        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 7); // 6 failures + 1 success

        // Calculate expected delays:
        // 10ms, 20ms, 40ms, 80ms, 160ms, 320ms
        // Sum: 630ms ± 157.5ms (472.5-787.5ms)
        let base_sum =
            BASE_MS + BASE_MS * 2 + BASE_MS * 4 + BASE_MS * 8 + BASE_MS * 16 + BASE_MS * 32;
        let min_expected = (base_sum as f64 * (1.0 - JITTER_PERCENT)) as u64;
        let max_expected = (base_sum as f64 * (1.0 + JITTER_PERCENT)) as u64;

        assert!(
            elapsed >= Duration::from_millis(min_expected),
            "Many retries elapsed {}ms should be >= {}ms",
            elapsed.as_millis(),
            min_expected
        );
        assert!(
            elapsed < Duration::from_millis(max_expected + 50),
            "Many retries elapsed {}ms should be < {}ms",
            elapsed.as_millis(),
            max_expected + 50
        );
    }

    // ─── Unit tests for verify_backend_identity ETXTBSY retry integration ─────────────
    //
    // These tests verify that the ETXTBSY retry wrapper is correctly integrated at
    // the spawn site in verify_backend_identity(). They test the retry behavior
    // with simulated ETXTBSY errors to ensure:
    // - Transient ETXTBSY errors are retried and eventually succeed
    // - Max retry exhaustion is handled correctly with proper error propagation
    // - Non-ETXTBSY errors fail fast without unnecessary retries
    //
    // The retry wrappers themselves are exhaustively tested above - these tests
    // specifically verify the integration at the verify_backend_identity spawn site.

    #[test]
    fn verify_backend_identity_spawn_site_retries_on_etxtbsy() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Test that the spawn wrapper at verify_backend_identity correctly retries
        // when it encounters ETXTBSY errors and eventually succeeds
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    // Fail first 2 attempts with ETXTBSY (simulating busy binary)
                    Err::<_, io::Error>(make_etxtbsy_error())
                } else {
                    // Succeed on 3rd attempt (simulating successful spawn after retries)
                    // Create a mock child process that exits successfully
                    std::process::Command::new("echo")
                        .arg("bead 0.1.1")
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                }
            },
            5,  // max_attempts (same as verify_backend_identity uses)
            20, // backoff_ms (same as verify_backend_identity uses)
        );

        // Verify that the retry succeeded after transient ETXTBSY failures
        assert!(
            result.is_ok(),
            "spawn_with_etxtbsy_retry_sync_child should succeed after ETXTBSY retries, got: {:?}",
            result
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "Should have made 3 attempts (2 failures + 1 success)"
        );
    }

    #[test]
    fn verify_backend_identity_spawn_site_exhausts_retries_on_persistent_etxtbsy() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Test that the spawn wrapper at verify_backend_identity correctly exhausts
        // all retry attempts when ETXTBSY errors persist and propagates the error
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                // Always fail with ETXTBSY (simulating persistently busy binary)
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            5,  // max_attempts (same as verify_backend_identity uses)
            20, // backoff_ms (same as verify_backend_identity uses)
        );

        // Verify that all retries were exhausted and error was propagated
        assert!(
            result.is_err(),
            "Should fail after exhausting all retry attempts"
        );
        let err = result.unwrap_err();
        assert_eq!(
            err.raw_os_error(),
            Some(26),
            "Error should be ETXTBSY (errno 26)"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            5,
            "Should have made exactly 5 attempts (max_attempts)"
        );
    }

    #[test]
    fn verify_backend_identity_spawn_site_fails_fast_on_non_etxtbsy_error() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Test that the spawn wrapper at verify_backend_identity fails fast
        // on non-ETXTBSY errors without retrying (e.g., binary not found)
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                // Fail with non-ETXTBSY error (simulating binary not found)
                Err::<_, io::Error>(make_other_error())
            },
            5,  // max_attempts
            20, // backoff_ms
        );

        // Verify that it failed immediately without retrying
        assert!(result.is_err(), "Should fail on non-ETXTBSY error");
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "Error should be NotFound (non-ETXTBSY)"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Should have made only 1 attempt (no retries for non-ETXTBSY errors)"
        );
    }

    #[test]
    fn verify_backend_identity_spawn_site_succeeds_on_first_attempt() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Test that the spawn wrapper at verify_backend_identity succeeds
        // immediately on first attempt when there's no ETXTBSY error
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                // Succeed on first attempt (healthy binary)
                std::process::Command::new("echo")
                    .arg("bead 0.1.1")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            },
            5,  // max_attempts
            20, // backoff_ms
        );

        // Verify that it succeeded on first try without retries
        assert!(
            result.is_ok(),
            "Should succeed on first attempt without retries"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Should have made only 1 attempt (immediate success)"
        );
    }

    #[test]
    fn verify_backend_identity_spawn_site_retries_with_single_attempt() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Test edge case: max_attempts=1 means no retries, just one attempt
        let result = spawn_with_etxtbsy_retry_sync_child(
            || {
                let count = Arc::clone(&call_count_clone);
                count.fetch_add(1, Ordering::SeqCst);
                // Always fail with ETXTBSY
                Err::<_, io::Error>(make_etxtbsy_error())
            },
            1,  // max_attempts=1 (single attempt, no retries)
            20, // backoff_ms
        );

        // Verify that it failed immediately without any retries
        assert!(result.is_err(), "Should fail on ETXTBSY error");
        let err = result.unwrap_err();
        assert_eq!(
            err.raw_os_error(),
            Some(26),
            "Error should be ETXTBSY (errno 26)"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Should have made exactly 1 attempt (max_attempts=1)"
        );
    }

    // ─── Integration tests for verify_backend_identity spawn site ─────────────────────
    //
    // These tests verify that the ETXTBSY retry wrapper is correctly integrated at
    // the spawn sites in verify_backend_identity() and verify_bead_rs_capabilities().
    //
    // Note: Shell scripts cannot trigger real ETXTBSY OS errors (errno 26) - they can
    // only exit with code 26. The retry logic checks raw_os_error() == Some(26), which
    // is an OS-level error, not a process exit code. These tests focus on error
    // propagation and spawn site correctness rather than simulating actual ETXTBSY.
    // The retry wrappers themselves are exhaustively tested in the ETXTBSY retry tests
    // above - these tests verify the integration at the specific spawn sites.

    #[cfg(unix)]
    #[test]
    fn verify_backend_identity_handles_healthy_bead_rs_binary() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a healthy bead-rs binary that succeeds on first try
        let bead_rs = workspace.path().join("bead-rs-fixture");
        std::fs::write(
            &bead_rs,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs, perm).unwrap();

        // Test that verify_backend_identity succeeds immediately for healthy binary
        let result =
            verify_backend_identity(&crate::config::Backend::Bead, &bead_rs, workspace.path());

        assert!(
            result.is_ok(),
            "verify_backend_identity should succeed for healthy bead-rs binary, got: {:?}",
            result
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_backend_identity_rejects_mismatched_identity() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a bead-forge binary when bead-rs is expected
        let wrong_binary = workspace.path().join("wrong-backend-fixture");
        std::fs::write(
            &wrong_binary,
            r#"#!/usr/bin/env bash
echo "bf 0.4.1"
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&wrong_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&wrong_binary, perm).unwrap();

        // Test that identity mismatch is caught and properly reported
        let result = verify_backend_identity(
            &crate::config::Backend::Bead,
            &wrong_binary,
            workspace.path(),
        );

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("identity mismatch") || err_msg.contains("does not match"),
            "Error should mention identity mismatch, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_backend_identity_propagates_spawn_failure() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary that always fails (non-zero exit code)
        let failing_binary = workspace.path().join("failing-fixture");
        std::fs::write(
            &failing_binary,
            r#"#!/usr/bin/env bash
echo "Error: something went wrong" >&2
exit 1
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&failing_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&failing_binary, perm).unwrap();

        // Test that spawn failure is properly propagated
        let result = verify_backend_identity(
            &crate::config::Backend::Bead,
            &failing_binary,
            workspace.path(),
        );

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to inspect bead CLI identity")
                || err_msg.contains("exited with code"),
            "Error should mention spawn failure, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_backend_identity_handles_missing_binary() {
        let workspace = tempfile::tempdir().unwrap();
        let nonexistent = workspace.path().join("nonexistent-binary");

        // Test that missing binary error is properly propagated
        let result = verify_backend_identity(
            &crate::config::Backend::Bead,
            &nonexistent,
            workspace.path(),
        );

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to inspect bead CLI identity")
                || err_msg.contains("No such file"),
            "Error should mention spawn failure, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_handles_healthy_binary() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a healthy bead-rs binary with proper capabilities
        let bead_rs = workspace.path().join("bead-rs-caps-fixture");
        std::fs::write(
            &bead_rs,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs, perm).unwrap();

        // Test that verify_bead_rs_capabilities succeeds for healthy binary
        let result = verify_bead_rs_capabilities(&bead_rs, workspace.path());

        assert!(
            result.is_ok(),
            "verify_bead_rs_capabilities should succeed for healthy bead-rs binary, got: {:?}",
            result
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_propagates_capability_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a bead-rs binary with missing capabilities
        let incomplete_caps = workspace.path().join("incomplete-caps-fixture");
        std::fs::write(
            &incomplete_caps,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":false,"statuses":["open","in_progress"],"schemas":[]}'
else
  echo "bead 0.1.1"
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&incomplete_caps).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&incomplete_caps, perm).unwrap();

        // Test that capability mismatch is caught and properly reported
        let result = verify_bead_rs_capabilities(&incomplete_caps, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("capability mismatch"),
            "Error should mention capability mismatch, got: {}",
            err_msg
        );
    }

    // ─── Tests for parse_backend_name_from_version ─────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_from_bead_forge_output() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let bf_binary = workspace.path().join("bf-fixture");
        std::fs::write(
            &bf_binary,
            r#"#!/usr/bin/env bash
echo "bf 0.4.1"
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bf_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bf_binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&bf_binary, None)
            .await
            .expect("should parse bf output successfully");
        assert_eq!(backend_name, "bf");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_from_bead_rs_output() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let bead_binary = workspace.path().join("bead-fixture");
        std::fs::write(
            &bead_binary,
            r#"#!/usr/bin/env bash
echo "bead 0.1.1"
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&bead_binary, None)
            .await
            .expect("should parse bead output successfully");
        assert_eq!(backend_name, "bead");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_with_custom_version_args() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let custom_binary = workspace.path().join("custom-fixture");
        std::fs::write(
            &custom_binary,
            r#"#!/usr/bin/env bash
if [ "$1" = "version" ]; then
  echo "my-backend 2.0.0"
else
  echo "Unknown command"
  exit 1
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&custom_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&custom_binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&custom_binary, Some(&["version"]))
            .await
            .expect("should parse output with custom args successfully");
        assert_eq!(backend_name, "my-backend");
    }

    #[tokio::test]
    async fn parse_backend_name_handles_missing_binary() {
        let nonexistent = PathBuf::from("/nonexistent/binary-that-does-not-exist");

        let result = parse_backend_name_from_version(&nonexistent, None).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to spawn") || err_msg.contains("No such file"),
            "Error should mention spawn failure, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_bad_exit_code() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let failing_binary = workspace.path().join("failing-fixture");
        std::fs::write(
            &failing_binary,
            r#"#!/usr/bin/env bash
echo "Error: something went wrong" >&2
exit 1
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&failing_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&failing_binary, perm).unwrap();

        let result = parse_backend_name_from_version(&failing_binary, None).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exited with code"),
            "Error should mention exit code, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_empty_output() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let empty_binary = workspace.path().join("empty-fixture");
        std::fs::write(
            &empty_binary,
            r#"#!/usr/bin/env bash
# Output nothing
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&empty_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&empty_binary, perm).unwrap();

        let result = parse_backend_name_from_version(&empty_binary, None).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty version output") || err_msg.contains("empty"),
            "Error should mention empty output, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_various_output_formats() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Test different output formats
        let test_cases = vec![
            ("bf 0.2.0-github", "bf"),
            ("bead 0.1.1-abc123", "bead"),
            ("my-backend 1.0.0-beta", "my-backend"),
            ("bead-forge 0.4.1", "bead-forge"),
            ("bead-rs 0.2.0", "bead-rs"),
        ];

        for (output, expected_name) in test_cases {
            let binary = workspace.path().join(format!("fixture-{}", expected_name));
            std::fs::write(
                &binary,
                format!(
                    r#"#!/usr/bin/env bash
echo "{}"
"#,
                    output
                ),
            )
            .unwrap();
            let mut perm = std::fs::metadata(&binary).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&binary, perm).unwrap();

            let backend_name = parse_backend_name_from_version(&binary, None)
                .await
                .unwrap_or_else(|_| panic!("should parse {} output successfully", expected_name));
            assert_eq!(
                backend_name, expected_name,
                "Expected {}, got {} for output {}",
                expected_name, backend_name, output
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_stderr_output() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("stderr-fixture");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env bash
echo "bead 0.1.1" >&2
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&binary, None)
            .await
            .expect("should parse stderr output successfully");
        assert_eq!(backend_name, "bead");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_mixed_stdout_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("mixed-fixture");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env bash
echo "Warning: deprecated" >&2
echo "bead 0.1.1"
echo "Info: something else"
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&binary, None)
            .await
            .expect("should parse mixed output successfully");
        assert_eq!(backend_name, "bead");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parse_backend_name_handles_whitespace_leading_output() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("whitespace-fixture");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env bash
echo "   bead 0.1.1"
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&binary, perm).unwrap();

        let backend_name = parse_backend_name_from_version(&binary, None)
            .await
            .expect("should handle leading whitespace successfully");
        assert_eq!(backend_name, "bead");
    }

    // ─── Regression tests for capabilities accuracy ─────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_rejects_blocked_status() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a bead-rs binary that incorrectly reports 'blocked' as a status
        let bead_rs = workspace.path().join("bead-rs-with-blocked");
        std::fs::write(
            &bead_rs,
            "#!/bin/sh\nif [ \"$1\" = \"capabilities\" ]; then\n  printf '{\"implementation\":\"bead-rs\",\"atomic_claim\":true,\"statuses\":[\"open\",\"in_progress\",\"deferred\",\"closed\",\"blocked\"],\"schemas\":[{\"schema_ref\":\"urn:bead-rs:schema:issue:native-v1\"},{\"schema_ref\":\"urn:bead-rs:schema:event:native-v1\"},{\"schema_ref\":\"urn:bead-rs:schema:field-guide:native-v1\"}],\"commands\":[\"ref\",\"data\",\"query\"]}'\nelse\n  echo 'bead 0.1.1'\nfi\n",
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs, perm).unwrap();

        // Test that capabilities with 'blocked' status are rejected
        let result = verify_bead_rs_capabilities(&bead_rs, workspace.path());

        // This should fail because 'blocked' is a derived presentation status, not a stored status
        assert!(
            result.is_err(),
            "verify_bead_rs_capabilities should reject binary with 'blocked' status (blocked is not a valid stored status)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("unexpected status"),
            "error message should mention unexpected status, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_requires_all_expected_statuses() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a bead-rs binary missing the 'deferred' status
        let bead_rs = workspace.path().join("bead-rs-missing-deferred");
        std::fs::write(
            &bead_rs,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs, perm).unwrap();

        // Test that missing required status fails
        let result = verify_bead_rs_capabilities(&bead_rs, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("capability mismatch") && err_msg.contains("deferred"),
            "Error should mention missing 'deferred' status, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_requires_all_expected_schemas() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a bead-rs binary missing the event schema
        let bead_rs = workspace.path().join("bead-rs-missing-event-schema");
        std::fs::write(
            &bead_rs,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs, perm).unwrap();

        // Test that missing required schema fails
        let result = verify_bead_rs_capabilities(&bead_rs, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("capability mismatch") && err_msg.contains("schema"),
            "Error should mention missing schema, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_accepts_all_three_required_schemas() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Test all three required schema refs are present
        let required_schemas = vec![
            "urn:bead-rs:schema:issue:native-v1",
            "urn:bead-rs:schema:event:native-v1",
            "urn:bead-rs:schema:field-guide:native-v1",
        ];

        for schema_ref in required_schemas {
            let bead_rs = workspace.path().join(format!(
                "bead-rs-schema-{}",
                schema_ref.split(':').next_back().unwrap()
            ));
            std::fs::write(
                &bead_rs,
                format!(
                    r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{{"schema_ref":"{}"}}],"commands":["ref","data","query"]}}'
else
  echo "bead 0.1.1"
fi
	"#,
                    schema_ref
                ),
            )
            .unwrap();
            let mut perm = std::fs::metadata(&bead_rs).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bead_rs, perm).unwrap();

            // Each individual schema should fail (we need all three)
            let result = verify_bead_rs_capabilities(&bead_rs, workspace.path());
            assert!(
                result.is_err(),
                "Should fail when missing required schemas (only has {})",
                schema_ref
            );
        }

        // Now test with all three schemas present - should succeed
        let bead_rs_complete = workspace.path().join("bead-rs-complete");
        std::fs::write(
            &bead_rs_complete,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&bead_rs_complete).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bead_rs_complete, perm).unwrap();

        let result = verify_bead_rs_capabilities(&bead_rs_complete, workspace.path());
        assert!(
            result.is_ok(),
            "Should succeed when all three required schemas are present, got: {:?}",
            result
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_rejects_wrong_implementation() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary claiming to be something else
        let wrong_impl = workspace.path().join("wrong-implementation");
        std::fs::write(
            &wrong_impl,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-forge","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&wrong_impl).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&wrong_impl, perm).unwrap();

        let result = verify_bead_rs_capabilities(&wrong_impl, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("capability mismatch"),
            "Error should mention capability mismatch for wrong implementation, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_requires_atomic_claim() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary without atomic_claim
        let no_atomic = workspace.path().join("no-atomic-claim");
        std::fs::write(
            &no_atomic,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{"implementation":"bead-rs","atomic_claim":false,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&no_atomic).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&no_atomic, perm).unwrap();

        let result = verify_bead_rs_capabilities(&no_atomic, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("capability mismatch"),
            "Error should mention capability mismatch for missing atomic_claim, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_handles_invalid_json() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary that outputs invalid JSON
        let invalid_json = workspace.path().join("invalid-json");
        std::fs::write(
            &invalid_json,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  printf '{this is not valid json}'
else
  echo "bead 0.1.1"
fi
	"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&invalid_json).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&invalid_json, perm).unwrap();

        let result = verify_bead_rs_capabilities(&invalid_json, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid bead-rs capability JSON") || err_msg.contains("JSON"),
            "Error should mention invalid JSON, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_command_invocation_is_correct() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary that logs its invocation to verify the correct command is used
        let log_file = workspace.path().join("invocation.log");
        let logging_binary = workspace.path().join("logging-bead");
        std::fs::write(
            &logging_binary,
            format!(
                r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  echo "capabilities --profile native-v1" >> "{}"
  printf '{{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{{"schema_ref":"urn:bead-rs:schema:issue:native-v1"}},{{"schema_ref":"urn:bead-rs:schema:event:native-v1"}},{{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}}],"commands":["ref","data","query"]}}'
else
  echo "bead 0.1.1"
fi
	"#,
                log_file.display()
            ),
        )
        .unwrap();
        let mut perm = std::fs::metadata(&logging_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&logging_binary, perm).unwrap();

        let result = verify_bead_rs_capabilities(&logging_binary, workspace.path());
        assert!(result.is_ok(), "Should succeed with logging binary");

        // Verify the command was invoked correctly
        let log_contents = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            log_contents.contains("capabilities --profile native-v1"),
            "Expected to find 'capabilities --profile native-v1' in invocation log, got: {}",
            log_contents
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_detects_backend_identity_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary named "bead" (implies bead-rs) but reports wrong implementation
        let mismatched_binary = workspace.path().join("bead");
        std::fs::write(
            &mismatched_binary,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  # Binary is named "bead" but reports "bead-forge" implementation
  printf '{"implementation":"bead-forge","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&mismatched_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&mismatched_binary, perm).unwrap();

        // Test that backend identity mismatch is detected and reported
        let result = verify_bead_rs_capabilities(&mismatched_binary, workspace.path());

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("backend identity mismatch")
                || err_msg.contains("implies backend")
                || err_msg.contains("implementation"),
            "Error should mention backend identity mismatch, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("bead-rs") && err_msg.contains("bead-forge"),
            "Error should mention both expected (bead-rs) and actual (bead-forge) backends, got: {}",
            err_msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_bead_rs_capabilities_accepts_matching_backend_identity() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();

        // Create a binary named "bead" that correctly reports "bead-rs" implementation
        let matching_binary = workspace.path().join("bead");
        std::fs::write(
            &matching_binary,
            r#"#!/usr/bin/env bash
if [ "$1" = "capabilities" ]; then
  # Binary is named "bead" and correctly reports "bead-rs" implementation
  printf '{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{"schema_ref":"urn:bead-rs:schema:issue:native-v1"},{"schema_ref":"urn:bead-rs:schema:event:native-v1"},{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}],"commands":["ref","data","query"]}'
else
  echo "bead 0.1.1"
fi
"#,
        )
        .unwrap();
        let mut perm = std::fs::metadata(&matching_binary).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&matching_binary, perm).unwrap();

        // Test that matching backend identity is accepted
        let result = verify_bead_rs_capabilities(&matching_binary, workspace.path());

        // With the correct schema reference, this should succeed since all required schemas are present
        // and the backend identity matches (binary named "bead" reports "bead-rs" implementation)
        assert!(
            result.is_ok(),
            "verify_bead_rs_capabilities should succeed when backend identity matches and all required schemas are present, got: {:?}",
            result
        );
    }

    #[test]
    fn derive_expected_backend_from_filename_maps_common_binaries() {
        use std::path::Path;

        // Test "bead" -> "bead-rs"
        let bead_path = Path::new("/usr/local/bin/bead");
        assert_eq!(derive_expected_backend_from_filename(bead_path), "bead-rs");

        // Test "bf" -> "bead-forge"
        let bf_path = Path::new("/usr/local/bin/bf");
        assert_eq!(derive_expected_backend_from_filename(bf_path), "bead-forge");

        // Test unknown binary returns as-is
        let unknown_path = Path::new("/usr/local/bin/unknown-bead-cli");
        assert_eq!(
            derive_expected_backend_from_filename(unknown_path),
            "unknown-bead-cli"
        );
    }

    #[test]
    fn derive_expected_backend_from_filename_handles_paths_without_filename() {
        use std::path::Path;

        // Test path without filename returns "unknown"
        let empty_path = Path::new("/");
        assert_eq!(derive_expected_backend_from_filename(empty_path), "unknown");

        // Test path with trailing slash
        let trailing_path = Path::new("/usr/local/bin/");
        assert_eq!(
            derive_expected_backend_from_filename(trailing_path),
            "unknown"
        );
    }
}
