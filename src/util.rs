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
}
