//! Utility functions for common operations.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Safely retrieve the HOME environment variable.
///
/// Returns `None` if HOME is not set, rather than panicking.
///
/// # Examples
///
/// ```no_run
/// use needle::util::get_home;
///
/// match get_home() {
///     Some(home) => println!("Home directory: {}", home),
///     None => println!("HOME not set"),
/// }
/// ```
pub fn get_home() -> Option<String> {
    env::var("HOME").ok()
}

/// Safely retrieve the HOME environment variable with a default value.
///
/// Returns the provided default if HOME is not set.
///
/// # Examples
///
/// ```no_run
/// use needle::util::get_home_or_default;
///
/// // Use "." as fallback
/// let home = get_home_or_default(".");
/// ```
pub fn get_home_or_default<S: Into<String>>(default: S) -> String {
    env::var("HOME").unwrap_or_else(|_| default.into())
}

/// Check if a path starts with a tilde-slash prefix.
///
/// Returns `true` if the path starts with exactly "~/", `false` otherwise.
/// This is a stricter check than just checking for "~" — it requires
/// the slash to be present.
///
/// # Arguments
///
/// * `path` - A path string to check
///
/// # Returns
///
/// * `bool` - `true` if path starts with "~/", `false` otherwise
///
/// # Examples
///
/// ```no_run
/// use needle::util::is_tilde_prefix;
///
/// assert!(is_tilde_prefix("~/foo"));
/// assert!(is_tilde_prefix("~/"));
/// assert!(!is_tilde_prefix("~foo"));  // No slash
/// assert!(!is_tilde_prefix("~"));      // Bare tilde
/// assert!(!is_tilde_prefix("foo"));
/// assert!(!is_tilde_prefix("/absolute/path"));
/// ```
///
/// # Edge Cases
///
/// * "~" alone returns `false` (no slash)
/// * "~foo" returns `false` (no slash)
/// * "~/foo" returns `true` (has slash)
/// * Empty string returns `false`
pub fn is_tilde_prefix(path: &str) -> bool {
    path.starts_with("~/")
}

/// Expand a tilde-slash path prefix to the HOME directory.
///
/// For paths starting with "~/", replaces the prefix with the HOME directory.
/// Also expands bare "~" to the HOME directory. If HOME is not set, returns
/// the path unchanged. Non-tilde paths are returned unchanged.
///
/// # Arguments
///
/// * `path` - A path string that may start with "~/" or be exactly "~"
///
/// # Returns
///
/// * `String` - The expanded path, or the original path if HOME is missing or
///   the path doesn't start with "~"
///
/// # Examples
///
/// ```no_run
/// use needle::util::expand_tilde;
///
/// // Assuming HOME=/home/coding
/// assert_eq!(expand_tilde("~"), "/home/coding");
/// assert_eq!(expand_tilde("~/foo"), "/home/coding/foo");
/// assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
/// assert_eq!(expand_tilde("relative/path"), "relative/path");
/// ```
///
/// # Edge Cases
///
/// * "~" alone expands to HOME directory (when HOME is set)
/// * "~" alone returns "~" unchanged (when HOME is not set)
/// * If HOME is not set, "~/foo" returns "~/foo" unchanged
/// * No double slashes: "~//foo" expands to "$HOME//foo" (preserves user input)
pub fn expand_tilde(path: &str) -> String {
    // Check if path is exactly ~ (bare tilde)
    if path == "~" {
        match get_home() {
            Some(home) => return home,
            None => return path.to_string(),
        }
    }

    // Check if path starts with ~/ (not just ~)
    if !path.starts_with("~/") {
        return path.to_string();
    }

    match get_home() {
        Some(home) => {
            // Replace "~/" with HOME + "/"
            // path[2..] skips the "~/" prefix
            format!("{}/{}", home.trim_end_matches('/'), &path[2..])
        }
        None => path.to_string(),
    }
}

/// Resolve the worker binary path with optional config override.
///
/// This function resolves the path to the worker binary by first checking
/// for an explicit config override, then falling back to the current executable.
///
/// # Arguments
///
/// * `worker_binary_path` - An optional path override from config. If `Some(path)`,
///   that path is used directly. If `None`, falls back to `std::env::current_exe()`.
///
/// # Returns
///
/// * `Result<PathBuf>` - The resolved binary path, or an error if resolution fails.
///
/// # Errors
///
/// * Returns an error if `worker_binary_path` is `None` and `std::env::current_exe()`
///   fails (which can happen in some restricted environments or when the binary
///   has been deleted/moved after launch).
///
/// # Examples
///
/// ```no_run
/// use needle::util::resolve_worker_binary_path;
/// use std::path::PathBuf;
///
/// // With explicit override
/// let override_path = Some(PathBuf::from("/custom/path/to/needle"));
/// let resolved = resolve_worker_binary_path(override_path.as_ref())
///     .expect("failed to resolve binary path");
/// assert_eq!(resolved, PathBuf::from("/custom/path/to/needle"));
///
/// // Without override (uses current_exe)
/// let resolved = resolve_worker_binary_path(None)
///     .expect("failed to resolve current executable");
/// ```
pub fn resolve_worker_binary_path(worker_binary_path: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = worker_binary_path {
        Ok(path.clone())
    } else {
        std::env::current_exe()
            .context("failed to resolve worker binary path: current_exe() failed")
    }
}

/// Build a cargo test command with configurable timeout.
///
/// This function constructs a `std::process::Command` that runs `cargo test`
/// with a timeout wrapper. The timeout command will terminate the test if it
/// exceeds the specified duration.
///
/// The command is structured as: `timeout <seconds> cargo test --all-targets -- --nocapture`
///
/// # Arguments
///
/// * `timeout_minutes` - Optional timeout in minutes. If `None`, defaults to 30 minutes.
///
/// # Returns
///
/// * `std::process::Command` - Configured command ready for execution via `spawn()` or `status()`.
///
/// # Examples
///
/// ```no_run
/// use needle::util::build_cargo_test_command;
/// use std::process::Command;
///
/// // With default 30-minute timeout
/// let cmd = build_cargo_test_command(None);
///
/// // With custom 45-minute timeout
/// let cmd = build_cargo_test_command(Some(45));
///
/// // Execute the command
/// let status = cmd.status().expect("failed to execute cargo test");
/// ```
///
/// # Timeout Behavior
///
/// - The `timeout` command will send `SIGTERM` to the cargo process when the timeout is reached.
/// - If the process does not exit within 1 second after `SIGTERM`, `SIGKILL` is sent.
/// - Exit code 124 indicates the timeout was triggered (see `timeout(1)` man page).
///
/// # Default Value
///
/// When `timeout_minutes` is `None`, the default is **30 minutes**.
pub fn build_cargo_test_command(timeout_minutes: Option<u64>) -> std::process::Command {
    let timeout_seconds = timeout_minutes.unwrap_or(30) * 60;

    let mut cmd = std::process::Command::new("timeout");
    cmd.arg(format!("{}", timeout_seconds))
        .arg("cargo")
        .arg("test")
        .arg("--all-targets")
        .arg("--")
        .arg("--nocapture");

    cmd
}

/// Capture the current system time in UTC as an ISO 8601 formatted string.
///
/// Returns the current UTC timestamp as an ISO 8601/RFC 3339 string.
/// The format is `2026-08-13T14:30:00Z` (UTC timezone indicated by `Z`).
///
/// This function handles all errors internally and never panics. If system
/// time is unavailable or chrono formatting fails (e.g., due to extreme
/// system clock values that overflow chrono's internal calculations), it
/// falls back to the Unix epoch timestamp (`1970-01-01T00:00:00Z`) and
/// logs an error message to stderr.
///
/// # Returns
///
/// * `String` - Current UTC timestamp in ISO 8601 format, or the epoch
///   timestamp as a fallback if time capture fails
///
/// # Fallback Behavior
///
/// When the system time cannot be retrieved (e.g., in restricted environments)
/// or chrono formatting fails, this function:
/// - Returns `"1970-01-01T00:00:00Z"` (Unix epoch)
/// - Logs an error message to stderr with failure details
/// - Never panics, ensuring the function is safe to call in production
///
/// # Examples
///
/// ```no_run
/// use needle::util::capture_timestamp;
///
/// let now = capture_timestamp();
/// println!("Current UTC time: {}", now);
/// // Output: "2026-08-13T14:30:00Z" (or epoch on failure)
/// ```
///
/// # Performance
///
/// This function performs a system call to get the current time and should
/// be called only when a timestamp is actually needed.
///
/// # Error Handling
///
/// For use cases that need to distinguish between success and failure,
/// use [`capture_timestamp_result()`] instead, which returns a `Result`.
pub fn capture_timestamp() -> String {
    capture_timestamp_result().unwrap_or_else(|e| {
        eprintln!("Failed to capture timestamp: {}. Using epoch fallback.", e);
        // Unix epoch as ISO 8601 timestamp (fallback when system time fails)
        "1970-01-01T00:00:00Z".to_string()
    })
}

/// Capture the current system time in UTC as an ISO 8601 formatted string.
///
/// Returns the current UTC timestamp as an ISO 8601/RFC 3339 string.
/// The format is `2026-08-13T14:30:00Z` (UTC timezone indicated by `Z`).
///
/// This is the fallible version of [`capture_timestamp()`] that returns
/// a `Result` instead of handling errors internally. Use this when you
/// need to distinguish between successful timestamp capture and failures.
///
/// # Returns
///
/// * `Result<String>` - Current UTC timestamp in ISO 8601 format, or an
///   error if time capture fails
///
/// # Errors
///
/// This function returns an error if:
/// - The system time cannot be retrieved (e.g., in restricted environments)
/// - Chrono formatting fails to produce an ISO 8601 string (e.g., due to
///   extreme system clock values)
///
/// # Examples
///
/// ```no_run
/// use needle::util::capture_timestamp_result;
///
/// match capture_timestamp_result() {
///     Ok(timestamp) => println!("Current UTC time: {}", timestamp),
///     Err(e) => eprintln!("Failed to capture timestamp: {}", e),
/// }
/// ```
///
/// # Performance
///
/// This function performs a system call to get the current time and should
/// be called only when a timestamp is actually needed.
pub fn capture_timestamp_result() -> Result<String> {
    // Use catch_unwind to handle any potential panics from chrono operations
    // This protects against extreme edge cases like system clock overflow
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let now = chrono::Utc::now();
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }));

    match result {
        Ok(timestamp) => Ok(timestamp),
        Err(_) => Err(anyhow::anyhow!(
            "chrono operation panicked - possibly due to extreme system clock values"
        )),
    }
}

/// Parse the backend name from a binary's version output.
///
/// Runs the specified binary with the given version command arguments,
/// captures stdout, and extracts the backend name (first word of output).
///
/// This function includes ETXTBSY (errno 26) retry logic to handle the race
/// condition where a binary written to disk immediately before execution
/// transiently reports "Text file busy".
///
/// # Arguments
///
/// * `binary_path` - Path to the binary to execute
/// * `version_args` - Arguments to pass for version check (e.g., `["--version"]`)
///
/// # Returns
///
/// * `Result<String>` - The extracted backend name, or an error if:
///   - Binary not found
///   - Binary exits with non-zero code
///   - Output is empty or unparseable
///
/// # Examples
///
/// ```no_run
/// use needle::util::parse_backend_name_from_version;
/// use std::path::Path;
///
/// # fn main() -> anyhow::Result<()> {
/// let backend = parse_backend_name_from_version(
///     Path::new("/usr/local/bin/bf"),
///     &["--version"]
/// )?;
/// assert_eq!(backend, "bf");
/// # Ok(())
/// # }
/// ```
pub fn parse_backend_name_from_version(
    binary_path: &std::path::Path,
    version_args: &[&str],
) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread;

    // Check if binary exists
    if !binary_path.exists() {
        anyhow::bail!(
            "binary not found at {}",
            binary_path.display()
        );
    }

    // ETXTBSY retry logic: retry with backoff when the kernel reports "Text file busy"
    const ETXTBSY_ERRNO: i32 = 26;
    let max_attempts = 5;
    let backoff_ms = 20;

    let mut last_err = None;
    for attempt in 0..max_attempts {
        let result = Command::new(binary_path)
            .args(version_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match result {
            Ok(output) => {
                // Check exit status
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!(
                        "binary {} exited with code {}: {}",
                        binary_path.display(),
                        output.status.code().unwrap_or(-1),
                        stderr.trim()
                    );
                }

                // Capture stdout
                let stdout = String::from_utf8_lossy(&output.stdout);
                let trimmed_output = stdout.trim();

                // Extract backend name (first word)
                let backend_name = trimmed_output
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "binary {} produced empty or unparseable version output",
                            binary_path.display()
                        )
                    })?;

                return Ok(backend_name.to_string());
            }
            Err(e) if e.raw_os_error() == Some(ETXTBSY_ERRNO) && attempt + 1 < max_attempts => {
                last_err = Some(e);
                thread::sleep(std::time::Duration::from_millis(backoff_ms));
            }
            Err(e) => return Err(e).with_context(|| {
                format!(
                    "failed to execute binary {} with version args {:?}",
                    binary_path.display(),
                    version_args
                )
            }),
        }
    }

    // All retries exhausted
    Err(last_err.expect("loop always sets last_err before exhausting max_attempts"))
        .with_context(|| {
            format!(
                "failed to execute binary {} after {} attempts (ETXTBSY)",
                binary_path.display(),
                max_attempts
            )
        })
}

/// Crate-wide test-only environment isolation.
///
/// `HOME` is process-global, but Rust runs unit tests as threads inside a
/// single process. Any test that calls `set_var("HOME", ..)` or
/// `remove_var("HOME")` therefore mutates state that every concurrently
/// running test observes. Before this module existed, several modules each
/// had their own private `Mutex` (or none at all), which provides no mutual
/// exclusion against the others — tests that merely *read* `HOME`
/// (`worker::tests`, `telemetry::tests`, tilde expansion) failed
/// nondeterministically depending on interleaving.
///
/// Every test that reads or writes `HOME`/`PATH` must go through
/// [`isolate_env`] or [`isolate_env_with_home`] so that exactly one such test
/// runs at a time and the previous values are always restored.
#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    /// The single process-wide lock guarding `HOME`/`PATH` in tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restores `HOME` and `PATH` to their captured values when dropped.
    pub(crate) struct EnvGuard {
        home: Option<OsString>,
        path: Option<OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore("HOME", self.home.take());
            restore("PATH", self.path.take());
        }
    }

    fn restore(key: &str, value: Option<OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    /// A private, per-caller NEEDLE home for tests.
    ///
    /// `WorkspaceConfig::default_home()` resolves to the operator's real
    /// `~/.needle`, whose `state/workers.json` is the *live fleet's* worker
    /// registry. Unit tests that construct a `Worker` therefore registered
    /// themselves into it and raced each other (and the running fleet) on a
    /// single read-modify-write file. Every test config must point somewhere
    /// private instead.
    ///
    /// Each call returns a fresh subdirectory, so concurrently running tests
    /// that share a worker id never touch the same registry file. The backing
    /// root is held in a `OnceLock` for the lifetime of the test process.
    pub(crate) fn isolated_home() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;

        static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        let root =
            ROOT.get_or_init(|| tempfile::tempdir().expect("failed to create test home root"));
        let dir = root
            .path()
            .join(format!("home-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(dir.join("state")).expect("failed to create test home state dir");
        dir
    }

    /// Acquire the environment lock and capture `HOME`/`PATH` for restoration.
    ///
    /// Hold the returned tuple for the whole test body; dropping it releases
    /// the lock and restores the environment. The lock is intentionally
    /// poison-tolerant: a panicking test must not wedge every later test.
    pub(crate) fn isolate_env() -> (MutexGuard<'static, ()>, EnvGuard) {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let guard = EnvGuard {
            home: std::env::var_os("HOME"),
            path: std::env::var_os("PATH"),
        };
        (lock, guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};
    use std::env;
    use std::path::PathBuf;

    #[test]
    fn test_is_tilde_prefix_with_tilde_slash() {
        // "~/foo" should return true (tilde prefix)
        assert!(is_tilde_prefix("~/foo"));
        assert!(is_tilde_prefix("~/"));
        assert!(is_tilde_prefix("~/Documents/file.txt"));
        assert!(is_tilde_prefix("~/path/to/file"));
    }

    #[test]
    fn test_is_tilde_prefix_without_tilde_slash() {
        // "foo" should return false (no tilde prefix)
        assert!(!is_tilde_prefix("foo"));
        assert!(!is_tilde_prefix("bar"));
        assert!(!is_tilde_prefix("relative/path"));
        assert!(!is_tilde_prefix("file.txt"));
    }

    #[test]
    fn test_is_tilde_prefix_tilde_without_slash() {
        // "~foo" (no slash) should return false
        assert!(!is_tilde_prefix("~foo"));
        assert!(!is_tilde_prefix("~username"));
        assert!(!is_tilde_prefix("~backup"));
        assert!(!is_tilde_prefix("~."));
        assert!(!is_tilde_prefix("~.."));
    }

    #[test]
    fn test_is_tilde_prefix_bare_tilde() {
        // "~" alone should return false (not "~/")
        assert!(!is_tilde_prefix("~"));
    }

    #[test]
    fn test_is_tilde_prefix_absolute_paths() {
        // Absolute paths should return false
        assert!(!is_tilde_prefix("/absolute/path"));
        assert!(!is_tilde_prefix("/usr/local/bin"));
        assert!(!is_tilde_prefix("/etc/config.json"));
        assert!(!is_tilde_prefix("/"));
    }

    #[test]
    fn test_is_tilde_prefix_empty_string() {
        // Empty string should return false
        assert!(!is_tilde_prefix(""));
    }

    #[test]
    fn test_is_tilde_prefix_whitespace_variants() {
        // Paths with leading whitespace should return false
        assert!(!is_tilde_prefix(" ~/foo")); // Space before tilde
        assert!(!is_tilde_prefix("  ~/foo")); // Multiple spaces
    }

    #[test]
    fn test_is_tilde_prefix_complex_paths() {
        // Test with more complex path patterns
        assert!(is_tilde_prefix("~/deep/nested/path/here"));
        assert!(is_tilde_prefix("~/config/settings.local.json"));
        assert!(is_tilde_prefix("~/project/src/main.rs"));

        // Test paths with tilde elsewhere but not at start
        assert!(!is_tilde_prefix("path/~foo"));
        assert!(!is_tilde_prefix("a/~/b"));
        assert!(!is_tilde_prefix("foo~bar"));
    }

    #[test]
    fn test_is_tilde_prefix_multiple_tildes() {
        // Test with multiple tildes
        assert!(is_tilde_prefix("~/~/path")); // Starts with "~/"
        assert!(!is_tilde_prefix("~~/path")); // Does not start with "~/"
        assert!(!is_tilde_prefix("~foo~")); // No slash
    }

    #[test]
    fn test_expand_tilde_with_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Set a known HOME value for testing
        env::set_var("HOME", "/home/testuser");

        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo");
        assert_eq!(
            expand_tilde("~/Documents/file.txt"),
            "/home/testuser/Documents/file.txt"
        );
        assert_eq!(expand_tilde("~"), "/home/testuser"); // "~" alone expands to HOME
        assert_eq!(expand_tilde(""), ""); // Empty string
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path"); // Absolute paths unchanged
        assert_eq!(expand_tilde("relative/path"), "relative/path"); // Relative paths unchanged
    }

    #[test]
    fn test_expand_tilde_without_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Remove HOME for this test
        env::remove_var("HOME");

        assert_eq!(expand_tilde("~/foo"), "~/foo"); // Returns unchanged when HOME is missing
        assert_eq!(expand_tilde("~"), "~"); // "~" returns unchanged when HOME is missing
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_expand_tilde_trailing_slash_in_home() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Test that trailing slashes in HOME are handled correctly
        env::set_var("HOME", "/home/testuser/");

        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo"); // No double slash
        assert_eq!(expand_tilde("~/bar/baz"), "/home/testuser/bar/baz");
    }

    #[test]
    fn test_expand_tilde_preserves_double_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // If the user types "~//foo", we preserve the double slash (it's their input)
        assert_eq!(expand_tilde("~//foo"), "/home/testuser//foo");
    }

    #[test]
    fn test_get_home_or_default() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/test");
        assert_eq!(get_home_or_default("fallback"), "/home/test");

        env::remove_var("HOME");
        assert_eq!(get_home_or_default("fallback"), "fallback");
    }

    #[test]
    fn test_expand_tilde_multi_level_paths() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Test single level
        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo");

        // Test two levels
        assert_eq!(expand_tilde("~/bar/baz"), "/home/testuser/bar/baz");

        // Test three levels
        assert_eq!(expand_tilde("~/a/b/c"), "/home/testuser/a/b/c");

        // Test four levels
        assert_eq!(
            expand_tilde("~/deep/nested/path/here"),
            "/home/testuser/deep/nested/path/here"
        );

        // Test mixed with file extensions
        assert_eq!(
            expand_tilde("~/project/src/main.rs"),
            "/home/testuser/project/src/main.rs"
        );

        // Test path with dots
        assert_eq!(
            expand_tilde("~/config/settings.local.json"),
            "/home/testuser/config/settings.local.json"
        );
    }

    #[test]
    fn test_expand_tilde_normal_cases() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Basic tilde expansion
        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo");

        // Two-level path
        assert_eq!(expand_tilde("~/bar/baz"), "/home/testuser/bar/baz");

        // Multi-level path
        assert_eq!(expand_tilde("~/a/b/c"), "/home/testuser/a/b/c");

        // Path with file extension
        assert_eq!(expand_tilde("~/doc.txt"), "/home/testuser/doc.txt");

        // Path with multiple extensions
        assert_eq!(
            expand_tilde("~/archive.tar.gz"),
            "/home/testuser/archive.tar.gz"
        );
    }

    #[test]
    fn test_expand_tilde_edge_case_tilde_without_slash() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // "~foo" should not be expanded (not a home path pattern)
        assert_eq!(expand_tilde("~foo"), "~foo");
        assert_eq!(expand_tilde("~username"), "~username");
        assert_eq!(expand_tilde("~backup"), "~backup");

        // "~" alone should expand to HOME
        assert_eq!(expand_tilde("~"), "/home/testuser");

        // "~." should not be expanded
        assert_eq!(expand_tilde("~."), "~.");
        assert_eq!(expand_tilde("~.."), "~..");
    }

    #[test]
    fn test_expand_tilde_edge_case_multiple_tildes() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Any path starting with "~/" is expanded, regardless of tildes elsewhere
        assert_eq!(expand_tilde("~/~/path"), "/home/testuser/~/path");
        assert_eq!(expand_tilde("~/a/~/b"), "/home/testuser/a/~/b");

        // Multiple tildes without slashes are not expanded
        assert_eq!(expand_tilde("~~/path"), "~~/path");
        assert_eq!(expand_tilde("~foo~"), "~foo~");
        assert_eq!(expand_tilde("~~/bar"), "~~/bar");

        // Mixed: first "~/path" expands, remaining tildes don't
        assert_eq!(expand_tilde("~/path/~other"), "/home/testuser/path/~other");
        assert_eq!(expand_tilde("~/~foo"), "/home/testuser/~foo");
    }

    #[test]
    fn test_expand_tilde_edge_case_empty_and_whitespace() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Empty string
        assert_eq!(expand_tilde(""), "");

        // Strings that look like they might start with tilde but don't
        assert_eq!(expand_tilde(" ~"), " ~"); // Space before tilde
        assert_eq!(expand_tilde("  ~/foo"), "  ~/foo"); // Multiple spaces
    }

    #[test]
    fn test_expand_tilde_edge_case_no_home_with_tilde_variants() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::remove_var("HOME");

        // Without HOME, all tilde variants are returned unchanged
        assert_eq!(expand_tilde("~"), "~"); // Bare tilde returns unchanged when HOME missing
        assert_eq!(expand_tilde("~/"), "~/");
        assert_eq!(expand_tilde("~/foo"), "~/foo");
        assert_eq!(expand_tilde("~foo"), "~foo");
        assert_eq!(expand_tilde("~/~/path"), "~/~/path");
    }

    /// Test bare tilde expansion behavior.
    ///
    /// This verifies that "~" alone expands to HOME when set, and returns unchanged
    /// when HOME is not set. This is the core fix for the bare tilde expansion issue.
    #[test]
    fn test_bare_tilde_expansion() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // With HOME set, bare tilde should expand to HOME
        env::set_var("HOME", "/home/testuser");
        assert_eq!(expand_tilde("~"), "/home/testuser");

        // With HOME unset, bare tilde should return unchanged
        env::remove_var("HOME");
        assert_eq!(expand_tilde("~"), "~");
    }

    /// Test that absolute paths without tilde prefix pass through unchanged.
    ///
    /// This is one of the acceptance criteria: verify "/abs/path" returns unchanged.
    #[test]
    fn test_absolute_paths_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Absolute paths should pass through unchanged
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("/usr/local/bin"), "/usr/local/bin");
        assert_eq!(expand_tilde("/etc/config.json"), "/etc/config.json");
        assert_eq!(expand_tilde("/"), "/");
        assert_eq!(expand_tilde("/var/log/app.log"), "/var/log/app.log");
    }

    /// Test that relative paths without tilde prefix pass through unchanged.
    ///
    /// This is one of the acceptance criteria: verify relative paths without tilde
    /// remain unchanged.
    #[test]
    fn test_relative_paths_unchanged() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        env::set_var("HOME", "/home/testuser");

        // Relative paths should pass through unchanged
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde("./current/dir"), "./current/dir");
        assert_eq!(expand_tilde("../parent/dir"), "../parent/dir");
        assert_eq!(expand_tilde("file.txt"), "file.txt");
        assert_eq!(
            expand_tilde("nested/deep/path/file.json"),
            "nested/deep/path/file.json"
        );
    }

    /// Test fallback behavior when HOME environment variable is not set.
    ///
    /// This is one of the acceptance criteria: verify missing HOME fallback behavior
    /// where tilde paths return unchanged.
    #[test]
    fn test_missing_home_fallback() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Explicitly remove HOME to test fallback behavior
        env::remove_var("HOME");

        // When HOME is missing, tilde-prefixed paths should return unchanged
        assert_eq!(expand_tilde("~/foo"), "~/foo");
        assert_eq!(expand_tilde("~/documents/file.txt"), "~/documents/file.txt");
        assert_eq!(expand_tilde("~/path/to/resource"), "~/path/to/resource");

        // Verify non-tilde paths still work correctly
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
    }

    /// Test that HOME manipulation in tests is properly isolated.
    ///
    /// This is one of the acceptance criteria: ensure proper test isolation for HOME
    /// manipulation. Each test should set up its own HOME state.
    #[test]
    fn test_home_isolation() {
        let (_env_lock, _env_guard) = crate::util::test_env::isolate_env();
        // Set HOME to a specific value
        env::set_var("HOME", "/home/testuser");
        assert_eq!(expand_tilde("~/test"), "/home/testuser/test");

        // Change HOME in the same test
        env::set_var("HOME", "/different/home");
        assert_eq!(expand_tilde("~/test"), "/different/home/test");

        // Remove HOME in the same test
        env::remove_var("HOME");
        assert_eq!(expand_tilde("~/test"), "~/test");

        // Restore HOME and verify it works again
        env::set_var("HOME", "/restored/home");
        assert_eq!(expand_tilde("~/test"), "/restored/home/test");
    }

    #[test]
    fn test_resolve_worker_binary_path_with_override() {
        let override_path = Some(PathBuf::from("/custom/path/to/needle"));
        let resolved = resolve_worker_binary_path(override_path.as_ref())
            .expect("failed to resolve binary path with override");
        assert_eq!(resolved, PathBuf::from("/custom/path/to/needle"));
    }

    #[test]
    fn test_resolve_worker_binary_path_without_override() {
        let resolved =
            resolve_worker_binary_path(None).expect("failed to resolve current executable");
        // current_exe() should always succeed in normal test environments
        // and will return the path to the test binary
        assert!(resolved.exists(), "resolved path should exist");
    }

    #[test]
    fn test_resolve_worker_binary_path_different_override() {
        let override_path = Some(PathBuf::from("/usr/local/bin/needle-worker"));
        let resolved = resolve_worker_binary_path(override_path.as_ref())
            .expect("failed to resolve binary path with override");
        assert_eq!(resolved, PathBuf::from("/usr/local/bin/needle-worker"));
    }

    #[test]
    fn test_resolve_worker_binary_path_relative_override() {
        let override_path = Some(PathBuf::from("./target/debug/needle"));
        let resolved = resolve_worker_binary_path(override_path.as_ref())
            .expect("failed to resolve binary path with relative override");
        assert_eq!(resolved, PathBuf::from("./target/debug/needle"));
    }

    #[test]
    fn test_build_cargo_test_command_default_timeout() {
        let cmd = build_cargo_test_command(None);

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        // Verify the arguments: ["1800", "cargo", "test", "--all-targets", "--", "--nocapture"]
        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "1800",
                "cargo",
                "test",
                "--all-targets",
                "--",
                "--nocapture"
            ]
        );
    }

    #[test]
    fn test_build_cargo_test_command_custom_timeout() {
        let cmd = build_cargo_test_command(Some(45));

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        // 45 minutes = 2700 seconds
        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "2700",
                "cargo",
                "test",
                "--all-targets",
                "--",
                "--nocapture"
            ]
        );
    }

    #[test]
    fn test_build_cargo_test_command_one_minute() {
        let cmd = build_cargo_test_command(Some(1));

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        // 1 minute = 60 seconds
        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec!["60", "cargo", "test", "--all-targets", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_build_cargo_test_command_zero_minutes() {
        let cmd = build_cargo_test_command(Some(0));

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        // 0 minutes = 0 seconds (edge case, but valid)
        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec!["0", "cargo", "test", "--all-targets", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_build_cargo_test_command_large_timeout() {
        let cmd = build_cargo_test_command(Some(120));

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        // 120 minutes = 7200 seconds (2 hours)
        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "7200",
                "cargo",
                "test",
                "--all-targets",
                "--",
                "--nocapture"
            ]
        );
    }

    #[test]
    fn test_build_cargo_test_command_structure() {
        let cmd = build_cargo_test_command(Some(15));

        // Verify the program is 'timeout'
        assert_eq!(cmd.get_program(), "timeout");

        let args: Vec<&str> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();

        // Verify the structure in detail
        assert_eq!(args[0], "900"); // 15 minutes = 900 seconds
        assert_eq!(args[1], "cargo");
        assert_eq!(args[2], "test");
        assert_eq!(args[3], "--all-targets");
        assert_eq!(args[4], "--");
        assert_eq!(args[5], "--nocapture");

        // Verify total argument count
        assert_eq!(args.len(), 6);
    }

    #[test]
    fn test_capture_timestamp_returns_string() {
        let timestamp = capture_timestamp();

        // Verify it returns a String type
        let _: String = timestamp;
    }

    #[test]
    fn test_capture_timestamp_is_iso8601_format() {
        let timestamp = capture_timestamp();

        // Verify the timestamp is in ISO 8601 format (RFC 3339)
        // Format: 2026-08-13T14:30:00Z
        assert!(
            timestamp.len() == 20,
            "ISO 8601 timestamp should be 20 characters"
        );
        assert!(
            timestamp.ends_with('Z'),
            "ISO 8601 UTC timestamp should end with 'Z'"
        );

        // Verify it can be parsed back as a DateTime
        timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp should be valid ISO 8601 format");
    }

    #[test]
    fn test_capture_timestamp_is_current() {
        let _before = chrono::Utc::now();
        let timestamp = capture_timestamp();
        let _after = chrono::Utc::now();

        // Parse the timestamp string and verify it's reasonably close to now
        let parsed = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp should be valid ISO 8601 format");

        // Allow a small window for timing variations (system clock adjustments, etc.)
        // The timestamp should be within 1 second of "now"
        let now = chrono::Utc::now();
        let diff_from_now = (now - parsed).num_seconds().abs();

        assert!(
            diff_from_now <= 1,
            "captured timestamp should be within 1 second of now, but was {} seconds off",
            diff_from_now
        );
    }

    #[test]
    fn test_capture_timestamp_consistency() {
        // Call the function multiple times and verify we get different timestamps
        let timestamp1 = capture_timestamp();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let timestamp2 = capture_timestamp();

        // Parse both timestamps and verify timestamp2 is later or equal
        let parsed1 = timestamp1
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp1 should be valid ISO 8601 format");
        let parsed2 = timestamp2
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp2 should be valid ISO 8601 format");

        assert!(
            parsed2 >= parsed1,
            "later timestamp should be greater or equal"
        );
    }

    #[test]
    fn test_capture_timestamp_result_returns_ok() {
        let result = capture_timestamp_result();

        // Should always return Ok in normal operation
        assert!(result.is_ok(), "capture_timestamp_result should return Ok");

        let timestamp = result.unwrap();
        // Verify the timestamp is valid ISO 8601
        timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp should be valid ISO 8601 format");
    }

    #[test]
    fn test_capture_timestamp_result_format_matches() {
        let result = capture_timestamp_result();
        let timestamp = result.expect("should return Ok");

        // Verify it matches the format from capture_timestamp()
        let direct = capture_timestamp();

        // Both should be valid ISO 8601
        let parsed_result = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("result timestamp should parse");
        let parsed_direct = direct
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("direct timestamp should parse");

        // They should be very close in time (within 1 second)
        let diff = (parsed_result - parsed_direct).num_seconds().abs();
        assert!(
            diff <= 1,
            "timestamps should be within 1 second, but were {} seconds apart",
            diff
        );
    }

    #[test]
    fn test_capture_timestamp_fallback_is_epoch() {
        // Verify the fallback timestamp is the Unix epoch
        let fallback = "1970-01-01T00:00:00Z";

        // Parse the fallback timestamp
        let parsed = fallback
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("fallback timestamp should be valid ISO 8601");

        // Verify it's the Unix epoch (timestamp 0)
        assert_eq!(parsed.timestamp(), 0, "fallback should be Unix epoch");
    }

    #[test]
    fn test_parse_backend_name_from_bf_version() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bf = tmp_dir.path().join("fake-bf");

        // Create a fake bf binary that outputs version info
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo "bf 0.4.1"
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&fake_bf).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_bf, perms).unwrap();

        let backend = parse_backend_name_from_version(&fake_bf, &["--version"])
            .expect("should parse bf version successfully");
        assert_eq!(backend, "bf");
    }

    #[test]
    fn test_parse_backend_name_from_bead_version() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_bead = tmp_dir.path().join("fake-bead");

        // Create a fake bead binary
        std::fs::write(
            &fake_bead,
            r#"#!/bin/sh
echo "bead 0.1.3 (commit 85f36ac)"
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&fake_bead).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_bead, perms).unwrap();

        let backend = parse_backend_name_from_version(&fake_bead, &["--version"])
            .expect("should parse bead version successfully");
        assert_eq!(backend, "bead");
    }

    #[test]
    fn test_parse_backend_name_various_output_formats() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let _fake_binary = tmp_dir.path().join("fake-binary");

        // Test with various output formats
        let test_outputs = vec![
            "backend-name 1.0.0",
            "my-backend 2.3.4-beta",
            "tool 0.1.0+build.sha1",
            "cli 3.0.0 (release)",
        ];

        for (i, output) in test_outputs.iter().enumerate() {
            let script = format!(
                r#"#!/bin/sh
echo "{}"
"#,
                output
            );

            let binary_path = tmp_dir.path().join(format!("fake-binary-{}", i));
            std::fs::write(&binary_path, script).unwrap();

            let mut perms = std::fs::metadata(&binary_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms).unwrap();

            let backend = parse_backend_name_from_version(&binary_path, &["--version"])
                .expect("should parse version successfully");

            let expected_name = output.split_whitespace().next().unwrap();
            assert_eq!(backend, expected_name);
        }
    }

    #[test]
    fn test_parse_backend_name_binary_not_found() {
        let non_existent = std::path::PathBuf::from("/nonexistent/path/to/binary");
        let result = parse_backend_name_from_version(&non_existent, &["--version"]);

        assert!(result.is_err(), "should fail when binary not found");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("binary not found") || error_msg.contains("No such file"));
    }

    #[test]
    fn test_parse_backend_name_bad_exit_code() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let failing_binary = tmp_dir.path().join("failing-binary");

        // Create a binary that exits with error code
        std::fs::write(
            &failing_binary,
            r#"#!/bin/sh
echo "Error: something went wrong" >&2
exit 1
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&failing_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&failing_binary, perms).unwrap();

        let result = parse_backend_name_from_version(&failing_binary, &["--version"]);

        assert!(result.is_err(), "should fail when binary exits with non-zero code");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("exited with code"));
    }

    #[test]
    fn test_parse_backend_name_empty_output() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let empty_binary = tmp_dir.path().join("empty-binary");

        // Create a binary that produces no output
        std::fs::write(
            &empty_binary,
            r#"#!/bin/sh
# Output nothing
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&empty_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&empty_binary, perms).unwrap();

        let result = parse_backend_name_from_version(&empty_binary, &["--version"]);

        assert!(result.is_err(), "should fail when binary produces empty output");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("empty") || error_msg.contains("unparseable"));
    }

    #[test]
    fn test_parse_backend_name_whitespace_only_output() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let whitespace_binary = tmp_dir.path().join("whitespace-binary");

        // Create a binary that produces only whitespace
        std::fs::write(
            &whitespace_binary,
            r#"#!/bin/sh
echo "   \t\n"
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&whitespace_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&whitespace_binary, perms).unwrap();

        let result = parse_backend_name_from_version(&whitespace_binary, &["--version"]);

        assert!(result.is_err(), "should fail when binary produces only whitespace");
    }

    #[test]
    fn test_parse_backend_name_custom_version_args() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let custom_binary = tmp_dir.path().join("custom-binary");

        // Create a binary that responds to -v instead of --version
        std::fs::write(
            &custom_binary,
            r#"#!/bin/sh
if [ "$1" = "-v" ]; then
    echo "custom-tool 1.2.3"
fi
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&custom_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&custom_binary, perms).unwrap();

        let backend = parse_backend_name_from_version(&custom_binary, &["-v"])
            .expect("should parse with custom version args");
        assert_eq!(backend, "custom-tool");
    }

    #[test]
    fn test_parse_backend_name_multiline_output() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let multiline_binary = tmp_dir.path().join("multiline-binary");

        // Create a binary that outputs multiline version info
        std::fs::write(
            &multiline_binary,
            r#"#!/bin/sh
echo "my-tool 2.0.0"
echo "Build metadata: some info"
echo "Copyright 2026"
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&multiline_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&multiline_binary, perms).unwrap();

        let backend = parse_backend_name_from_version(&multiline_binary, &["--version"])
            .expect("should parse first line of multiline output");
        assert_eq!(backend, "my-tool");
    }

    #[test]
    fn test_parse_backend_name_handles_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = tempfile::tempdir().unwrap();
        let noisy_binary = tmp_dir.path().join("noisy-binary");

        // Create a binary that outputs to both stdout and stderr
        std::fs::write(
            &noisy_binary,
            r#"#!/bin/sh
echo "Debug: starting up..." >&2
echo "noisy-tool 1.0.0"
echo "Warning: deprecated flag" >&2
"#,
        )
        .unwrap();

        let mut perms = std::fs::metadata(&noisy_binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&noisy_binary, perms).unwrap();

        let backend = parse_backend_name_from_version(&noisy_binary, &["--version"])
            .expect("should parse backend name from stdout despite stderr output");
        assert_eq!(backend, "noisy-tool");
    }

    #[test]
    fn test_capture_timestamp_exact_iso8601_format() {
        // This test verifies the exact ISO 8601/RFC 3339 format specification
        let timestamp = capture_timestamp();

        // Parse the timestamp to validate its format
        let parsed = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp should be valid ISO 8601 format");

        // Re-format with the exact same specification and verify it matches
        let reformatted = parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(
            timestamp, reformatted,
            "timestamp should match exact RFC 3339 format with seconds precision and UTC suffix"
        );
    }

    #[test]
    fn test_capture_timestamp_utc_timezone_designation() {
        // This test verifies that timestamps are explicitly in UTC (ending with 'Z')
        let timestamp = capture_timestamp();

        // Must end with 'Z' to indicate UTC timezone
        assert!(
            timestamp.ends_with('Z'),
            "UTC timestamp must end with 'Z' timezone indicator, got: {}",
            timestamp
        );

        // Parse and verify it's interpreted as UTC
        let parsed = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("should parse as UTC datetime");
        assert_eq!(
            parsed.timezone(),
            chrono::Utc,
            "parsed timestamp should be in UTC timezone"
        );
    }

    #[test]
    fn test_capture_timestamp_no_fractional_seconds() {
        // This test verifies that fractional seconds are NOT included
        // Format should be "2026-08-13T14:30:00Z", not "2026-08-13T14:30:00.123Z"
        let timestamp = capture_timestamp();

        // The timestamp should not contain a decimal point (which would indicate fractional seconds)
        assert!(
            !timestamp.contains('.'),
            "timestamp should not contain fractional seconds (no decimal point), got: {}",
            timestamp
        );

        // Verify format is exactly "YYYY-MM-DDTHH:MM:SSZ" (20 characters)
        assert_eq!(
            timestamp.len(),
            20,
            "timestamp should be exactly 20 characters (YYYY-MM-DDTHH:MM:SSZ), got: {} with length {}",
            timestamp,
            timestamp.len()
        );
    }

    #[test]
    fn test_capture_timestamp_string_roundtrip() {
        // This test verifies that a timestamp can be converted to a string and back
        let timestamp = capture_timestamp();

        // Parse the string back to a DateTime
        let parsed = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("timestamp should be valid ISO 8601 format");

        // Convert back to string with the same format
        let converted = parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        // The roundtrip should preserve the exact string
        assert_eq!(
            timestamp, converted,
            "timestamp roundtrip should preserve exact string representation"
        );
    }

    #[test]
    fn test_capture_timestamp_result_and_capture_timestamp_consistency() {
        // This test verifies that both functions return the same format and behavior
        let timestamp1 = capture_timestamp();
        let result = capture_timestamp_result();

        assert!(
            result.is_ok(),
            "capture_timestamp_result should return Ok in normal operation"
        );

        let timestamp2 = result.unwrap();

        // Both should be valid ISO 8601 strings
        let parsed1 = timestamp1
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("capture_timestamp result should parse");
        let parsed2 = timestamp2
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("capture_timestamp_result should parse");

        // Both should use the same format specification
        let formatted1 = parsed1.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let formatted2 = parsed2.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        assert_eq!(
            timestamp1, formatted1,
            "capture_timestamp should use correct format"
        );
        assert_eq!(
            timestamp2, formatted2,
            "capture_timestamp_result should use correct format"
        );
    }

    #[test]
    fn test_capture_timestamp_rapid_calls_return_monotonic_values() {
        // This test verifies that rapid calls return monotonically increasing timestamps
        let timestamps: Vec<String> = (0..10)
            .map(|_| {
                let ts = capture_timestamp();
                std::thread::sleep(std::time::Duration::from_millis(10));
                ts
            })
            .collect();

        // Parse all timestamps
        let parsed: Vec<chrono::DateTime<chrono::Utc>> = timestamps
            .iter()
            .map(|ts| {
                ts.parse::<chrono::DateTime<chrono::Utc>>()
                    .expect("should parse timestamp")
            })
            .collect();

        // Verify monotonicity (each timestamp should be >= the previous)
        for i in 1..parsed.len() {
            assert!(
                parsed[i] >= parsed[i - 1],
                "timestamp {} should be >= timestamp {}, got {} < {}",
                i,
                i - 1,
                parsed[i].to_rfc3339(),
                parsed[i - 1].to_rfc3339()
            );
        }
    }

    #[test]
    fn test_capture_timestamp_epoch_fallback_value() {
        // This test documents the exact fallback value used when timestamp capture fails
        let fallback = "1970-01-01T00:00:00Z";

        // Verify it's valid ISO 8601
        let parsed = fallback
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("fallback should be valid ISO 8601");

        // Verify it's exactly the Unix epoch (timestamp 0)
        assert_eq!(parsed.timestamp(), 0, "fallback should be Unix epoch");

        // Verify it uses the correct format
        let reformatted = parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(
            fallback, reformatted,
            "fallback should use correct RFC 3339 format"
        );
    }

    #[test]
    fn test_capture_timestamp_components() {
        // This test verifies that timestamp components are in the expected ranges
        let timestamp = capture_timestamp();
        let parsed = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("should parse timestamp");

        // Verify year is reasonable (2020-2030)
        assert!(
            parsed.year() >= 2020 && parsed.year() <= 2030,
            "year should be between 2020 and 2030, got {}",
            parsed.year()
        );

        // Verify month is valid (1-12)
        assert!(
            parsed.month() >= 1 && parsed.month() <= 12,
            "month should be 1-12, got {}",
            parsed.month()
        );

        // Verify day is valid (1-31)
        assert!(
            parsed.day() >= 1 && parsed.day() <= 31,
            "day should be 1-31, got {}",
            parsed.day()
        );

        // Verify hour is valid (0-23)
        assert!(
            parsed.hour() <= 23,
            "hour should be 0-23, got {}",
            parsed.hour()
        );

        // Verify minute is valid (0-59)
        assert!(
            parsed.minute() <= 59,
            "minute should be 0-59, got {}",
            parsed.minute()
        );

        // Verify second is valid (0-59)
        assert!(
            parsed.second() <= 59,
            "second should be 0-59, got {}",
            parsed.second()
        );

        // Verify nanoseconds are 0 (we use SecondsFormat::Secs)
        assert_eq!(
            parsed.nanosecond(),
            0,
            "nanoseconds should be 0 when using SecondsFormat::Secs, got {}",
            parsed.nanosecond()
        );
    }

    #[test]
    fn test_capture_timestamp_result_returns_result_type() {
        // This test verifies that capture_timestamp_result returns the correct Result type
        let result: Result<String, anyhow::Error> = capture_timestamp_result();

        // Should return Ok in normal operation
        assert!(result.is_ok(), "should return Ok(String)");

        let timestamp = result.unwrap();
        // The value should be a valid timestamp
        let _ = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("should be valid ISO 8601");
    }

    #[test]
    fn test_capture_timestamp_handles_panic_gracefully() {
        // This test documents that the function uses catch_unwind to handle chrono panics
        // In normal operation with a valid system clock, this should never panic
        // If chrono does panic (e.g., due to extreme system clock values), catch_unwind
        // converts the panic into an Err, and capture_timestamp returns the epoch fallback

        // Call the function - it should never panic
        let timestamp = capture_timestamp();

        // Should always return a valid string
        assert!(!timestamp.is_empty());
        let _ = timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("should be valid ISO 8601 even if fallback was used");
    }

    #[test]
    fn test_capture_timestamp_multiple_calls_consistency() {
        // This test verifies that multiple calls to capture_timestamp all return
        // strings in the correct format, regardless of timing variations

        for i in 0..20 {
            let timestamp = capture_timestamp();

            // Each timestamp should be valid ISO 8601
            let _parsed = timestamp
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap_or_else(|e| panic!("iteration {}: should parse, got error: {}", i, e));

            // Each should end with 'Z'
            assert!(
                timestamp.ends_with('Z'),
                "iteration {}: should end with 'Z', got: {}",
                i,
                timestamp
            );

            // Each should be exactly 20 characters
            assert_eq!(
                timestamp.len(),
                20,
                "iteration {}: should be 20 characters, got: {} with length {}",
                i,
                timestamp,
                timestamp.len()
            );

            // Each should have no fractional seconds
            assert!(
                !timestamp.contains('.'),
                "iteration {}: should not contain fractional seconds, got: {}",
                i,
                timestamp
            );

            // Small delay to advance time
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn test_capture_timestamp_format_specification_compliance() {
        // This test verifies compliance with the documented format specification:
        // "The format is `2026-08-13T14:30:00Z` (UTC timezone indicated by `Z`)"

        let timestamp = capture_timestamp();

        // Test the exact format with a regex-like pattern check
        // Format: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(
            timestamp.chars().nth(4),
            Some('-'),
            "should have '-' after year"
        );
        assert_eq!(
            timestamp.chars().nth(7),
            Some('-'),
            "should have '-' after month"
        );
        assert_eq!(
            timestamp.chars().nth(10),
            Some('T'),
            "should have 'T' between date and time"
        );
        assert_eq!(
            timestamp.chars().nth(13),
            Some(':'),
            "should have ':' after hour"
        );
        assert_eq!(
            timestamp.chars().nth(16),
            Some(':'),
            "should have ':' after minute"
        );
        assert_eq!(
            timestamp.chars().nth(19),
            Some('Z'),
            "should have 'Z' at the end (UTC indicator)"
        );
    }

    #[test]
    fn test_capture_timestamp_result_error_path_documentation() {
        // This test documents the error path behavior for capture_timestamp_result
        // In normal operation, this function returns Ok, but it can return Err if:
        // 1. The system time cannot be retrieved (e.g., in restricted environments)
        // 2. Chrono formatting fails (e.g., due to extreme system clock values)
        //
        // When Err is returned, capture_timestamp() handles it by:
        // 1. Returning the epoch fallback: "1970-01-01T00:00:00Z"
        // 2. Logging an error message to stderr

        // In normal test environment, should always return Ok
        let result = capture_timestamp_result();
        assert!(result.is_ok(), "normal operation should return Ok");

        // If it were to return Err, capture_timestamp() would handle it:
        // let timestamp = capture_timestamp(); // would return epoch fallback
        // assert_eq!(timestamp, "1970-01-01T00:00:00Z");
    }
}
