//! Integration tests for strand configuration path tilde expansion.
//!
//! These tests validate that tilde-prefixed paths in strand configuration fields
//! are correctly expanded to the HOME directory during config loading, with proper
//! tempdir isolation to avoid contaminating the real user environment.
//!
//! Follows the test isolation policy from NEEDLE/CLAUDE.md: all tests use isolated
//! HOME directories and proper HOME locking via the HOME_LOCK mutex.

use needle::config::Config;
use serial_test::serial;
use std::env;
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
// strands.explore.workspace_root tilde expansion tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.explore.workspace_root configuration field.
#[tokio::test]
#[serial]
async fn explore_workspace_root_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: ~/repos
    let yaml = r#"
strands:
  explore:
    workspace_root: ~/repos
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = PathBuf::from(&isolated_home).join("repos");
    assert_eq!(
        config.strands.explore.workspace_root, expected,
        "strands.explore.workspace_root should expand tilde correctly"
    );

    println!("✓ strands.explore.workspace_root tilde expansion test passed");
}

/// Test that non-tilde paths in strands.explore.workspace_root pass through unchanged.
#[tokio::test]
#[serial]
async fn explore_workspace_root_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test 1: Absolute path without tilde
    let yaml = r#"
strands:
  explore:
    workspace_root: /opt/repos
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.explore.workspace_root,
        PathBuf::from("/opt/repos"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
strands:
  explore:
    workspace_root: relative/repos
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.explore.workspace_root,
        PathBuf::from("relative/repos"),
        "relative path should pass through unchanged"
    );

    println!("✓ strands.explore.workspace_root non-tilde paths pass through unchanged");
}

/// Test strands.explore.workspace_root with bare tilde.
#[tokio::test]
#[serial]
async fn explore_workspace_root_tilde_expansion_bare_tilde() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: bare tilde (~ alone)
    let yaml = r#"
strands:
  explore:
    workspace_root: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.explore.workspace_root,
        PathBuf::from(&isolated_home),
        "bare tilde should expand to home directory"
    );

    println!("✓ strands.explore.workspace_root bare tilde expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// strands.explore.workspaces tilde expansion tests (Vec<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.explore.workspaces configuration field (Vec<PathBuf>).
#[tokio::test]
#[serial]
async fn explore_workspaces_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: multiple tilde paths in vector
    let yaml = r#"
strands:
  explore:
    workspaces:
      - ~/repo1
      - ~/repo2
      - ~/nested/deep/repo3
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("repo1"),
        PathBuf::from(&isolated_home).join("repo2"),
        PathBuf::from(&isolated_home).join("nested/deep/repo3"),
    ];

    assert_eq!(
        config.strands.explore.workspaces, expected,
        "strands.explore.workspaces should expand all tilde paths in vector"
    );

    println!("✓ strands.explore.workspaces tilde expansion test passed");
}

/// Test that non-tilde paths in strands.explore.workspaces pass through unchanged.
#[tokio::test]
#[serial]
async fn explore_workspaces_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: mixed absolute and relative paths
    let yaml = r#"
strands:
  explore:
    workspaces:
      - /absolute/repo1
      - relative/repo2
      - /opt/repos/repo3
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from("/absolute/repo1"),
        PathBuf::from("relative/repo2"),
        PathBuf::from("/opt/repos/repo3"),
    ];

    assert_eq!(
        config.strands.explore.workspaces, expected,
        "non-tilde paths should pass through unchanged"
    );

    println!("✓ strands.explore.workspaces non-tilde paths pass through unchanged");
}

/// Test strands.explore.workspaces with mixed tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn explore_workspaces_mixed_tilde_non_tilde_paths() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: mixed tilde and absolute paths
    let yaml = r#"
strands:
  explore:
    workspaces:
      - ~/repo1
      - /absolute/repo2
      - ~/repo3
      - relative/repo4
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("repo1"),
        PathBuf::from("/absolute/repo2"),
        PathBuf::from(&isolated_home).join("repo3"),
        PathBuf::from("relative/repo4"),
    ];

    assert_eq!(
        config.strands.explore.workspaces, expected,
        "mixed tilde/non-tilde paths should be handled correctly"
    );

    println!("✓ strands.explore.workspaces mixed tilde/non-tilde paths test passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// strands.weave.exclude_workspaces tilde expansion tests (Vec<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.weave.exclude_workspaces configuration field.
#[tokio::test]
#[serial]
async fn weave_exclude_workspaces_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: multiple tilde paths in exclude vector
    let yaml = r#"
strands:
  weave:
    exclude_workspaces:
      - ~/private/repo1
      - ~/work/internal
      - ~/secret
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("private/repo1"),
        PathBuf::from(&isolated_home).join("work/internal"),
        PathBuf::from(&isolated_home).join("secret"),
    ];

    assert_eq!(
        config.strands.weave.exclude_workspaces, expected,
        "strands.weave.exclude_workspaces should expand all tilde paths"
    );

    println!("✓ strands.weave.exclude_workspaces tilde expansion test passed");
}

/// Test that non-tilde paths in strands.weave.exclude_workspaces pass through unchanged.
#[tokio::test]
#[serial]
async fn weave_exclude_workspaces_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute paths in exclude vector
    let yaml = r#"
strands:
  weave:
    exclude_workspaces:
      - /opt/private/repo1
      - /var/lib/internal
      - /tmp/secret
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from("/opt/private/repo1"),
        PathBuf::from("/var/lib/internal"),
        PathBuf::from("/tmp/secret"),
    ];

    assert_eq!(
        config.strands.weave.exclude_workspaces, expected,
        "non-tilde exclude paths should pass through unchanged"
    );

    println!("✓ strands.weave.exclude_workspaces non-tilde paths pass through unchanged");
}

/// Test strands.weave.exclude_workspaces with mixed tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn weave_exclude_workspaces_mixed_tilde_non_tilde_paths() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: mixed tilde and absolute paths in exclude vector
    let yaml = r#"
strands:
  weave:
    exclude_workspaces:
      - ~/private/repo1
      - /opt/public/repo2
      - ~/work/internal
      - relative/exclude
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("private/repo1"),
        PathBuf::from("/opt/public/repo2"),
        PathBuf::from(&isolated_home).join("work/internal"),
        PathBuf::from("relative/exclude"),
    ];

    assert_eq!(
        config.strands.weave.exclude_workspaces, expected,
        "mixed exclude paths should be handled correctly"
    );

    println!("✓ strands.weave.exclude_workspaces mixed tilde/non-tilde paths test passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// strands.splice.report_workspace tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.splice.report_workspace configuration field.
#[tokio::test]
#[serial]
async fn splice_report_workspace_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
strands:
  splice:
    report_workspace: ~/reports/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("reports/workspace"));
    assert_eq!(
        config.strands.splice.report_workspace, expected,
        "strands.splice.report_workspace should expand tilde in Some"
    );

    println!("✓ strands.splice.report_workspace tilde expansion test passed");
}

/// Test that None in strands.splice.report_workspace remains None after expansion.
#[tokio::test]
#[serial]
async fn splice_report_workspace_none_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: None (field not set)
    let yaml = r#"
strands:
  splice: {}
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert!(
        config.strands.splice.report_workspace.is_none(),
        "None should remain None after expansion"
    );

    println!("✓ strands.splice.report_workspace None remains None");
}

/// Test that non-tilde paths in strands.splice.report_workspace pass through unchanged.
#[tokio::test]
#[serial]
async fn splice_report_workspace_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
strands:
  splice:
    report_workspace: /absolute/path/to/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/absolute/path/to/workspace"));
    assert_eq!(
        config.strands.splice.report_workspace, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ strands.splice.report_workspace non-tilde paths pass through unchanged");
}

/// Test that bare tilde in YAML is interpreted as null for optional fields.
///
/// In YAML, a bare `~` is interpreted as null/None, not as the string "~".
/// This is expected YAML behavior - users who want the home directory should
/// use an explicit path like `~/.needle` instead.
#[tokio::test]
#[serial]
async fn splice_report_workspace_bare_tilde_is_none() {
    let _guard = HomeGuard::isolate();

    // Test: bare tilde (~ alone) - YAML interprets this as null
    let yaml = r#"
strands:
  splice:
    report_workspace: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // YAML interprets bare ~ as null, so the field is None
    assert!(
        config.strands.splice.report_workspace.is_none(),
        "bare tilde in YAML is interpreted as null, not as a path"
    );

    println!("✓ strands.splice.report_workspace bare tilde correctly interpreted as null");
}

// ──────────────────────────────────────────────────────────────────────────────
// strands.learning.global_learnings_file tilde expansion tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.learning.global_learnings_file configuration field.
#[tokio::test]
#[serial]
async fn learning_global_learnings_file_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: ~/learnings/global.json
    let yaml = r#"
strands:
  learning:
    global_learnings_file: ~/learnings/global.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = PathBuf::from(&isolated_home).join("learnings/global.json");
    assert_eq!(
        config.strands.learning.global_learnings_file, expected,
        "strands.learning.global_learnings_file should expand tilde correctly"
    );

    println!("✓ strands.learning.global_learnings_file tilde expansion test passed");
}

/// Test that non-tilde paths in strands.learning.global_learnings_file pass through unchanged.
#[tokio::test]
#[serial]
async fn learning_global_learnings_file_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test 1: Absolute path without tilde
    let yaml = r#"
strands:
  learning:
    global_learnings_file: /var/lib/needle/learnings/global.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.learning.global_learnings_file,
        PathBuf::from("/var/lib/needle/learnings/global.json"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
strands:
  learning:
    global_learnings_file: relative/learnings.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.learning.global_learnings_file,
        PathBuf::from("relative/learnings.json"),
        "relative path should pass through unchanged"
    );

    println!("✓ strands.learning.global_learnings_file non-tilde paths pass through unchanged");
}

/// Test strands.learning.global_learnings_file with bare tilde.
#[tokio::test]
#[serial]
async fn learning_global_learnings_file_tilde_expansion_bare_tilde() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: bare tilde (~ alone)
    let yaml = r#"
strands:
  learning:
    global_learnings_file: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.strands.learning.global_learnings_file,
        PathBuf::from(&isolated_home),
        "bare tilde should expand to home directory"
    );

    println!("✓ strands.learning.global_learnings_file bare tilde expansion passed");
}

/// Test strands.learning.global_learnings_file with nested tilde path.
#[tokio::test]
#[serial]
async fn learning_global_learnings_file_tilde_expansion_nested_path() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: ~/.needle/learnings/global.json
    let yaml = r#"
strands:
  learning:
    global_learnings_file: ~/.needle/learnings/global.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = PathBuf::from(&isolated_home).join(".needle/learnings/global.json");
    assert_eq!(
        config.strands.learning.global_learnings_file, expected,
        "nested tilde path should expand correctly"
    );

    println!("✓ strands.learning.global_learnings_file nested tilde path expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// Combined tests - multiple strand path fields
// ──────────────────────────────────────────────────────────────────────────────

/// Test that multiple strand path fields can use tilde expansion simultaneously.
#[tokio::test]
#[serial]
async fn multiple_strand_fields_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: multiple strand fields with tilde paths
    let yaml = r#"
strands:
  explore:
    workspace_root: ~/repos
    workspaces:
      - ~/repo1
      - ~/repo2
  weave:
    exclude_workspaces:
      - ~/private/repo3
  splice:
    report_workspace: ~/reports/workspace
  learning:
    global_learnings_file: ~/learnings/global.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify all fields expanded correctly
    assert_eq!(
        config.strands.explore.workspace_root,
        PathBuf::from(&isolated_home).join("repos"),
        "explore.workspace_root should be expanded"
    );

    assert_eq!(
        config.strands.explore.workspaces,
        vec![
            PathBuf::from(&isolated_home).join("repo1"),
            PathBuf::from(&isolated_home).join("repo2"),
        ],
        "explore.workspaces should be expanded"
    );

    assert_eq!(
        config.strands.weave.exclude_workspaces,
        vec![PathBuf::from(&isolated_home).join("private/repo3")],
        "weave.exclude_workspaces should be expanded"
    );

    assert_eq!(
        config.strands.splice.report_workspace,
        Some(PathBuf::from(&isolated_home).join("reports/workspace")),
        "splice.report_workspace should be expanded"
    );

    assert_eq!(
        config.strands.learning.global_learnings_file,
        PathBuf::from(&isolated_home).join("learnings/global.json"),
        "learning.global_learnings_file should be expanded"
    );

    println!("✓ multiple strand fields tilde expansion test passed");
}

/// Test that strand path fields work correctly with mixed tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn strand_fields_mixed_tilde_non_tilde_paths() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: mixed tilde and absolute paths across multiple fields
    let yaml = r#"
strands:
  explore:
    workspace_root: /opt/repos
    workspaces:
      - ~/repo1
      - /absolute/repo2
  weave:
    exclude_workspaces:
      - ~/private/repo3
      - /opt/public/repo4
  splice:
    report_workspace: /reports/workspace
  learning:
    global_learnings_file: ~/learnings/global.json
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify mixed paths handled correctly
    assert_eq!(
        config.strands.explore.workspace_root,
        PathBuf::from("/opt/repos"),
        "absolute path should pass through"
    );

    assert_eq!(
        config.strands.explore.workspaces,
        vec![
            PathBuf::from(&isolated_home).join("repo1"),
            PathBuf::from("/absolute/repo2"),
        ],
        "mixed tilde/absolute paths should be handled correctly"
    );

    assert_eq!(
        config.strands.weave.exclude_workspaces,
        vec![
            PathBuf::from(&isolated_home).join("private/repo3"),
            PathBuf::from("/opt/public/repo4"),
        ],
        "mixed exclude paths should be handled correctly"
    );

    assert_eq!(
        config.strands.splice.report_workspace,
        Some(PathBuf::from("/reports/workspace")),
        "absolute path should pass through in Option"
    );

    assert_eq!(
        config.strands.learning.global_learnings_file,
        PathBuf::from(&isolated_home).join("learnings/global.json"),
        "tilde path should be expanded"
    );

    println!("✓ strand fields mixed tilde/non-tilde paths test passed");
}
