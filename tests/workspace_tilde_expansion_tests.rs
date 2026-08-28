//! Integration tests for workspace.home and workspace.default tilde expansion.
//!
//! These tests validate that tilde-prefixed paths in workspace.home and
//! workspace.default configuration fields are correctly expanded to the
//! HOME directory during config loading, with proper tempdir isolation
//! to avoid contaminating the real user environment.
//!
//! Follows the test isolation policy from NEEDLE/CLAUDE.md: all tests
//! use isolated HOME directories and proper HOME locking.

use needle::config::Config;
use needle::util::expand_tilde;
use serial_test::serial;
use std::env;
use std::fs;
use std::path::PathBuf;

// ──────────────────────────────────────────────────────────────────────────────
// Test isolation infrastructure
// ──────────────────────────────────────────────────────────────────────────────

/// Guard that restores HOME to its original value when dropped.
///
/// This prevents tests from writing to the live fleet's state directory
/// (`~/.needle/state/heartbeats/`). Each test gets its own temporary HOME
/// so heartbeats and other state files don't contaminate the real environment.
///
/// HOME is process-wide state, so isolation must also be mutually exclusive: two
/// tests isolating at once clobber each other and whichever sets HOME last wins for
/// both, making `~` expand to the other test's tempdir. `#[serial]` cannot prevent
/// this -- it only orders serial-marked tests against each other, so any non-serial
/// test racing one still corrupts it. The lock below makes isolation exclusive.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the HOME lock for tests that set HOME manually rather than via HomeGuard.
///
/// `#[serial]` is not sufficient on its own: it orders serial-marked tests against each
/// other, but the ~24 HomeGuard users are not serial, so a guard dropping in another
/// thread restores HOME mid-test. That is how a tilde expansion resolved to the real
/// `/root` under CI. Hold this for the whole test, before touching HOME.
fn lock_home() -> std::sync::MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct HomeGuard {
    _temp_dir: tempfile::TempDir,
    original_home: Option<std::ffi::OsString>,
    // Declared last so it is released only after Drop::drop has restored HOME.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeGuard {
    /// Isolates the test's HOME directory to a temp directory.
    ///
    /// Returns a guard that restores the original HOME value when dropped.
    /// Use this in any test that creates a HealthMonitor or Worker, as both
    /// may call `dirs_or_home()` which reads HOME directly.
    fn isolate() -> Self {
        // Take the lock before touching HOME. Recover from poisoning rather than
        // propagating it: one panicking test must not cascade into every other test
        // that isolates HOME.
        let lock = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let original_home = std::env::var_os("HOME");
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir for test HOME");
        let temp_path = temp_dir.path().to_path_buf();

        std::env::set_var("HOME", &temp_path);

        HomeGuard {
            _temp_dir: temp_dir,
            original_home,
            _lock: lock,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// workspace.home tilde expansion tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in workspace.home configuration field.
///
/// This test validates that tilde-prefixed paths in workspace.home are
/// correctly expanded to the HOME directory during config loading.
#[tokio::test]
#[serial]
async fn workspace_home_tilde_expansion() {
    use needle::util::expand_tilde;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a .needle directory in the isolated home
    let needle_dir = isolated_home.join(".needle");
    fs::create_dir_all(&needle_dir).expect("failed to create .needle dir");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test basic tilde expansion function for workspace.home
    let tilde_path = "~/.needle";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        isolated_home.join(".needle").to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test that tilde expansion works in config context for workspace.home
    let yaml = format!(
        r#"
workspace:
  home: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion after expand_tildes() call
    let mut config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes when expand_tildes() is called
    config.expand_tildes();

    // Verify the workspace.home was expanded
    assert!(
        config.workspace.home.starts_with(&isolated_home),
        "workspace.home should be expanded to isolated home, got: {}",
        config.workspace.home.display()
    );

    assert_eq!(
        config.workspace.home,
        isolated_home.join(".needle"),
        "workspace.home should match expected expanded path"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ workspace.home tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  ~/.needle -> {}", config.workspace.home.display());
}

/// Test that non-tilde paths in workspace.home pass through unchanged.
#[tokio::test]
#[serial]
async fn workspace_home_non_tilde_paths_unchanged() {
    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test 1: Absolute path without tilde
    let yaml = r#"
workspace:
  home: /absolute/path/to/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.home,
        PathBuf::from("/absolute/path/to/needle"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
workspace:
  home: relative/path/to/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.home,
        PathBuf::from("relative/path/to/needle"),
        "relative path should pass through unchanged"
    );

    // Test 3: Absolute path with root but no tilde
    let yaml = r#"
workspace:
  home: /opt/needle/home
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.home,
        PathBuf::from("/opt/needle/home"),
        "opt path should pass through unchanged"
    );

    println!("✓ workspace.home non-tilde paths pass through unchanged");
}

/// Test workspace.home with trailing slash in tilde path.
#[tokio::test]
#[serial]
async fn workspace_home_tilde_expansion_trailing_slash() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: ~/.needle/ with trailing slash
    let yaml = r#"
workspace:
  home: ~/.needle/
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Trailing slash should be preserved (current behavior)
    let expected = isolated_home.join(".needle/");
    assert_eq!(
        config.workspace.home, expected,
        "~/.needle/ should expand with trailing slash preserved"
    );

    println!("✓ workspace.home tilde expansion with trailing slash passed");
}

/// Test workspace.home with bare tilde.
#[tokio::test]
#[serial]
async fn workspace_home_tilde_expansion_bare_tilde() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: bare tilde (~ alone)
    let yaml = r#"
workspace:
  home: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.home, isolated_home,
        "bare tilde should expand to home directory"
    );

    println!("✓ workspace.home bare tilde expansion passed");
}

/// Test workspace.home with nested tilde path.
#[tokio::test]
#[serial]
async fn workspace_home_tilde_expansion_nested_path() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: ~/some/deep/nested/path
    let yaml = r#"
workspace:
  home: ~/some/deep/nested/path
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = isolated_home.join("some/deep/nested/path");
    assert_eq!(
        config.workspace.home, expected,
        "nested tilde path should expand correctly"
    );

    println!("✓ workspace.home nested tilde path expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// workspace.default tilde expansion tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in workspace.default configuration field.
///
/// This test validates that tilde-prefixed paths in workspace.default are
/// correctly expanded to the HOME directory during config loading.
#[tokio::test]
#[serial]
async fn workspace_default_tilde_expansion() {
    use needle::util::expand_tilde;

    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    // Create a workspaces directory in the isolated home
    let workspaces_dir = isolated_home.join("workspaces");
    fs::create_dir_all(&workspaces_dir).expect("failed to create workspaces dir");

    // Save the original HOME and set our isolated home
    let original_home = env::var("HOME").ok();
    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test basic tilde expansion function for workspace.default
    let tilde_path = "~/workspaces";
    let expanded = expand_tilde(tilde_path);

    assert_eq!(
        expanded,
        isolated_home.join("workspaces").to_str().unwrap(),
        "tilde path should be expanded to isolated home"
    );

    // Test that tilde expansion works in config context for workspace.default
    let yaml = format!(
        r#"
workspace:
  default: {}
"#,
        tilde_path
    );

    // Load config - this should trigger tilde expansion after expand_tildes() call
    let mut config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");

    // The config should expand tildes when expand_tildes() is called
    config.expand_tildes();

    // Verify the workspace.default was expanded
    assert!(
        config.workspace.default.starts_with(&isolated_home),
        "workspace.default should be expanded to isolated home, got: {}",
        config.workspace.default.display()
    );

    assert_eq!(
        config.workspace.default,
        isolated_home.join("workspaces"),
        "workspace.default should match expected expanded path"
    );

    // Restore original HOME
    if let Some(home) = original_home {
        env::set_var("HOME", home);
    } else {
        env::remove_var("HOME");
    }

    println!("✓ workspace.default tilde expansion test passed");
    println!("  Isolated home: {}", isolated_home.display());
    println!("  ~/workspaces -> {}", config.workspace.default.display());
}

/// Test that non-tilde paths in workspace.default pass through unchanged.
#[tokio::test]
#[serial]
async fn workspace_default_non_tilde_paths_unchanged() {
    // Create a completely isolated temp directory for this test
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test 1: Absolute path without tilde
    let yaml = r#"
workspace:
  default: /absolute/path/to/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        PathBuf::from("/absolute/path/to/workspace"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
workspace:
  default: relative/path/to/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        PathBuf::from("relative/path/to/workspace"),
        "relative path should pass through unchanged"
    );

    // Test 3: Current directory (.)
    let yaml = r#"
workspace:
  default: .
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        PathBuf::from("."),
        "current directory . should pass through unchanged"
    );

    println!("✓ workspace.default non-tilde paths pass through unchanged");
}

/// Test workspace.default with trailing slash in tilde path.
#[tokio::test]
#[serial]
async fn workspace_default_tilde_expansion_trailing_slash() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: ~/workspaces/ with trailing slash
    let yaml = r#"
workspace:
  default: ~/workspaces/
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Trailing slash should be preserved (current behavior)
    let expected = isolated_home.join("workspaces/");
    assert_eq!(
        config.workspace.default, expected,
        "~/workspaces/ should expand with trailing slash preserved"
    );

    println!("✓ workspace.default tilde expansion with trailing slash passed");
}

/// Test workspace.default with bare tilde.
#[tokio::test]
#[serial]
async fn workspace_default_tilde_expansion_bare_tilde() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: bare tilde (~ alone)
    let yaml = r#"
workspace:
  default: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default, isolated_home,
        "bare tilde should expand to home directory"
    );

    println!("✓ workspace.default bare tilde expansion passed");
}

/// Test workspace.default with nested tilde path.
#[tokio::test]
#[serial]
async fn workspace_default_tilde_expansion_nested_path() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: ~/dev/projects/myproject
    let yaml = r#"
workspace:
  default: ~/dev/projects/myproject
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = isolated_home.join("dev/projects/myproject");
    assert_eq!(
        config.workspace.default, expected,
        "nested tilde path should expand correctly"
    );

    println!("✓ workspace.default nested tilde path expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// Combined tests - both workspace.home and workspace.default
// ──────────────────────────────────────────────────────────────────────────────

/// Test that both workspace.home and workspace.default can use tilde expansion
/// simultaneously in the same config.
#[tokio::test]
#[serial]
async fn workspace_both_fields_tilde_expansion() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: both fields with tilde paths
    let yaml = r#"
workspace:
  default: ~/workspaces
  home: ~/.needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        isolated_home.join("workspaces"),
        "workspace.default should be expanded"
    );

    assert_eq!(
        config.workspace.home,
        isolated_home.join(".needle"),
        "workspace.home should be expanded"
    );

    println!("✓ both workspace.home and workspace.default tilde expansion passed");
}

/// Test that workspace.home and workspace.default work correctly with mixed
/// tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn workspace_mixed_tilde_non_tilde_paths() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let isolated_home = temp_dir.path().join("fake-home");
    fs::create_dir_all(&isolated_home).expect("failed to create fake home");

    let _home_lock = lock_home();
    env::set_var("HOME", &isolated_home);

    // Test: workspace.home with tilde, workspace.default with absolute path
    let yaml = r#"
workspace:
  default: /opt/workspaces
  home: ~/.needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        PathBuf::from("/opt/workspaces"),
        "workspace.default absolute path should pass through unchanged"
    );

    assert_eq!(
        config.workspace.home,
        isolated_home.join(".needle"),
        "workspace.home tilde should be expanded"
    );

    // Test: workspace.home with absolute path, workspace.default with tilde
    let yaml = r#"
workspace:
  default: ~/workspaces
  home: /var/lib/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.workspace.default,
        isolated_home.join("workspaces"),
        "workspace.default tilde should be expanded"
    );

    assert_eq!(
        config.workspace.home,
        PathBuf::from("/var/lib/needle"),
        "workspace.home absolute path should pass through unchanged"
    );

    println!("✓ mixed tilde/non-tilde paths for workspace fields passed");
}
