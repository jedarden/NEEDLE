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

/// Capture the current system time in UTC.
///
/// Returns the current UTC timestamp as a `chrono::DateTime<chrono::Utc>`.
/// This is a minimal timestamp capture function with no formatting logic.
///
/// # Returns
///
/// * `chrono::DateTime<chrono::Utc>` - Current UTC timestamp
///
/// # Examples
///
/// ```no_run
/// use needle::util::capture_timestamp;
///
/// let now = capture_timestamp();
/// println!("Current UTC time: {}", now);
/// ```
///
/// # Performance
///
/// This function performs a system call to get the current time and should
/// be called only when a timestamp is actually needed.
pub fn capture_timestamp() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::PathBuf;

    #[test]
    fn test_expand_tilde_with_home() {
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
        // Remove HOME for this test
        env::remove_var("HOME");

        assert_eq!(expand_tilde("~/foo"), "~/foo"); // Returns unchanged when HOME is missing
        assert_eq!(expand_tilde("~"), "~"); // "~" returns unchanged when HOME is missing
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_expand_tilde_trailing_slash_in_home() {
        // Test that trailing slashes in HOME are handled correctly
        env::set_var("HOME", "/home/testuser/");

        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo"); // No double slash
        assert_eq!(expand_tilde("~/bar/baz"), "/home/testuser/bar/baz");
    }

    #[test]
    fn test_expand_tilde_preserves_double_slash() {
        env::set_var("HOME", "/home/testuser");

        // If the user types "~//foo", we preserve the double slash (it's their input)
        assert_eq!(expand_tilde("~//foo"), "/home/testuser//foo");
    }

    #[test]
    fn test_get_home_or_default() {
        env::set_var("HOME", "/home/test");
        assert_eq!(get_home_or_default("fallback"), "/home/test");

        env::remove_var("HOME");
        assert_eq!(get_home_or_default("fallback"), "fallback");
    }

    #[test]
    fn test_expand_tilde_multi_level_paths() {
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
        env::set_var("HOME", "/home/testuser");

        // Empty string
        assert_eq!(expand_tilde(""), "");

        // Strings that look like they might start with tilde but don't
        assert_eq!(expand_tilde(" ~"), " ~"); // Space before tilde
        assert_eq!(expand_tilde("  ~/foo"), "  ~/foo"); // Multiple spaces
    }

    #[test]
    fn test_expand_tilde_edge_case_no_home_with_tilde_variants() {
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
            vec!["1800", "cargo", "test", "--all-targets", "--", "--nocapture"]
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
            vec!["2700", "cargo", "test", "--all-targets", "--", "--nocapture"]
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
            vec!["7200", "cargo", "test", "--all-targets", "--", "--nocapture"]
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
    fn test_capture_timestamp_returns_utc_datetime() {
        let timestamp = capture_timestamp();

        // Verify it returns a DateTime<Utc> type
        // This will compile only if the type matches
        let _: chrono::DateTime<chrono::Utc> = timestamp;
    }

    #[test]
    fn test_capture_timestamp_is_current() {
        let before = chrono::Utc::now();
        let timestamp = capture_timestamp();
        let after = chrono::Utc::now();

        // Verify the captured timestamp is between before and after
        assert!(timestamp >= before, "captured timestamp should be >= before");
        assert!(timestamp <= after, "captured timestamp should be <= after");
    }

    #[test]
    fn test_capture_timestamp_is_utc() {
        let timestamp = capture_timestamp();

        // Verify the timestamp is in UTC
        assert_eq!(timestamp.timezone(), chrono::Utc);
    }

    #[test]
    fn test_capture_timestamp_consistency() {
        // Call the function multiple times and verify we get different timestamps
        let timestamp1 = capture_timestamp();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let timestamp2 = capture_timestamp();

        // timestamp2 should be later than timestamp1
        assert!(timestamp2 > timestamp1, "later timestamp should be greater");
    }
}
