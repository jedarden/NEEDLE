//! Utility functions for common operations.

use std::env;

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
/// If HOME is not set, returns the path unchanged. Non-tilde paths are returned
/// unchanged.
///
/// # Arguments
///
/// * `path` - A path string that may start with "~/"
///
/// # Returns
///
/// * `String` - The expanded path, or the original path if HOME is missing or
///   the path doesn't start with "~/"
///
/// # Examples
///
/// ```no_run
/// use needle::util::expand_tilde;
///
/// // Assuming HOME=/home/coding
/// assert_eq!(expand_tilde("~/foo"), "/home/coding/foo");
/// assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
/// assert_eq!(expand_tilde("relative/path"), "relative/path");
/// ```
///
/// # Edge Cases
///
/// * "~" alone (without slash) is returned unchanged
/// * If HOME is not set, "~/foo" returns "~/foo" unchanged
/// * No double slashes: "~//foo" expands to "$HOME//foo" (preserves user input)
pub fn expand_tilde(path: &str) -> String {
    // Only expand paths starting with "~/", not "~" alone
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_expand_tilde_with_home() {
        // Set a known HOME value for testing
        env::set_var("HOME", "/home/testuser");

        assert_eq!(expand_tilde("~/foo"), "/home/testuser/foo");
        assert_eq!(
            expand_tilde("~/Documents/file.txt"),
            "/home/testuser/Documents/file.txt"
        );
        assert_eq!(expand_tilde("~"), "~"); // "~" alone is not expanded
        assert_eq!(expand_tilde(""), ""); // Empty string
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path"); // Absolute paths unchanged
        assert_eq!(expand_tilde("relative/path"), "relative/path"); // Relative paths unchanged
    }

    #[test]
    fn test_expand_tilde_without_home() {
        // Remove HOME for this test
        env::remove_var("HOME");

        assert_eq!(expand_tilde("~/foo"), "~/foo"); // Returns unchanged when HOME is missing
        assert_eq!(expand_tilde("~"), "~");
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

        // "~" alone should not be expanded
        assert_eq!(expand_tilde("~"), "~");

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
        assert_eq!(expand_tilde("~"), "~");
        assert_eq!(expand_tilde("~/"), "~/");
        assert_eq!(expand_tilde("~/foo"), "~/foo");
        assert_eq!(expand_tilde("~foo"), "~foo");
        assert_eq!(expand_tilde("~/~/path"), "~/~/path");
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
        assert_eq!(expand_tilde("nested/deep/path/file.json"), "nested/deep/path/file.json");
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
}
