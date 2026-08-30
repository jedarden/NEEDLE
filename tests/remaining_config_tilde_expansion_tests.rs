//! Integration tests for remaining config path fields that use tilde expansion.
//!
//! These tests validate that tilde-prefixed paths in other config sections
//! (worker, agent, bead_cli, post_push_ci, telemetry, health, supervisor,
//! prompt, self_modification) are correctly expanded to the HOME directory
//! during config loading, with proper tempdir isolation to avoid contaminating
//! the real user environment.
//!
//! Follows the test isolation policy from NEEDLE/CLAUDE.md: all tests use
//! isolated HOME directories and proper HOME locking via the HOME_LOCK mutex.

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
// worker.worker_binary_path tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in worker.worker_binary_path configuration field.
#[tokio::test]
#[serial]
async fn worker_binary_path_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
worker:
  worker_binary_path: ~/bin/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("bin/needle"));
    assert_eq!(
        config.worker.worker_binary_path, expected,
        "worker.worker_binary_path should expand tilde in Some"
    );

    println!("✓ worker.worker_binary_path tilde expansion test passed");
}

/// Test that None in worker.worker_binary_path remains None after expansion.
#[tokio::test]
#[serial]
async fn worker_binary_path_none_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: None (field not set)
    let yaml = r#"
worker: {}
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert!(
        config.worker.worker_binary_path.is_none(),
        "None should remain None after expansion"
    );

    println!("✓ worker.worker_binary_path None remains None");
}

/// Test that non-tilde paths in worker.worker_binary_path pass through unchanged.
#[tokio::test]
#[serial]
async fn worker_binary_path_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
worker:
  worker_binary_path: /usr/local/bin/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/usr/local/bin/needle"));
    assert_eq!(
        config.worker.worker_binary_path, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ worker.worker_binary_path non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// agent.adapters_dir tilde expansion tests (PathBuf)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in agent.adapters_dir configuration field.
#[tokio::test]
#[serial]
async fn agent_adapters_dir_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: ~/adapters
    let yaml = r#"
agent:
  adapters_dir: ~/adapters
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = PathBuf::from(&isolated_home).join("adapters");
    assert_eq!(
        config.agent.adapters_dir, expected,
        "agent.adapters_dir should expand tilde correctly"
    );

    println!("✓ agent.adapters_dir tilde expansion test passed");
}

/// Test that non-tilde paths in agent.adapters_dir pass through unchanged.
#[tokio::test]
#[serial]
async fn agent_adapters_dir_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test 1: Absolute path without tilde
    let yaml = r#"
agent:
  adapters_dir: /usr/local/lib/needle/adapters
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.agent.adapters_dir,
        PathBuf::from("/usr/local/lib/needle/adapters"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
agent:
  adapters_dir: relative/adapters
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.agent.adapters_dir,
        PathBuf::from("relative/adapters"),
        "relative path should pass through unchanged"
    );

    println!("✓ agent.adapters_dir non-tilde paths pass through unchanged");
}

/// Test agent.adapters_dir with bare tilde.
#[tokio::test]
#[serial]
async fn agent_adapters_dir_tilde_expansion_bare_tilde() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: bare tilde (~ alone)
    let yaml = r#"
agent:
  adapters_dir: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.agent.adapters_dir,
        PathBuf::from(&isolated_home),
        "bare tilde should expand to home directory"
    );

    println!("✓ agent.adapters_dir bare tilde expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// bead_cli.path tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in bead_cli.path configuration field.
#[tokio::test]
#[serial]
async fn bead_cli_path_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
bead_cli:
  backend: bead-rs
  path: ~/.local/bin/bead
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join(".local/bin/bead"));
    assert_eq!(
        config.bead_cli.path, expected,
        "bead_cli.path should expand tilde in Some"
    );

    println!("✓ bead_cli.path tilde expansion test passed");
}

/// Test that non-tilde paths in bead_cli.path pass through unchanged.
#[tokio::test]
#[serial]
async fn bead_cli_path_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
bead_cli:
  backend: bead-rs
  path: /usr/local/bin/bead
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/usr/local/bin/bead"));
    assert_eq!(
        config.bead_cli.path, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ bead_cli.path non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// post_push_ci.state_dir tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in post_push_ci.state_dir configuration field.
#[tokio::test]
#[serial]
async fn post_push_ci_state_dir_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
post_push_ci:
  state_dir: ~/.needle/ci-state
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join(".needle/ci-state"));
    assert_eq!(
        config.post_push_ci.state_dir, expected,
        "post_push_ci.state_dir should expand tilde in Some"
    );

    println!("✓ post_push_ci.state_dir tilde expansion test passed");
}

/// Test that non-tilde paths in post_push_ci.state_dir pass through unchanged.
#[tokio::test]
#[serial]
async fn post_push_ci_state_dir_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
post_push_ci:
  state_dir: /var/lib/needle/ci-state
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/var/lib/needle/ci-state"));
    assert_eq!(
        config.post_push_ci.state_dir, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ post_push_ci.state_dir non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// telemetry.file_sink.log_dir tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in telemetry.file_sink.log_dir configuration field.
#[tokio::test]
#[serial]
async fn telemetry_log_dir_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
telemetry:
  file_sink:
    log_dir: ~/logs/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("logs/needle"));
    assert_eq!(
        config.telemetry.file_sink.log_dir, expected,
        "telemetry.file_sink.log_dir should expand tilde in Some"
    );

    println!("✓ telemetry.file_sink.log_dir tilde expansion test passed");
}

/// Test that non-tilde paths in telemetry.file_sink.log_dir pass through unchanged.
#[tokio::test]
#[serial]
async fn telemetry_log_dir_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
telemetry:
  file_sink:
    log_dir: /var/log/needle
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/var/log/needle"));
    assert_eq!(
        config.telemetry.file_sink.log_dir, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ telemetry.file_sink.log_dir non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// health.heartbeat_dir tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in health.heartbeat_dir configuration field.
#[tokio::test]
#[serial]
async fn health_heartbeat_dir_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
health:
  heartbeat_dir: ~/.needle/state/heartbeats
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join(".needle/state/heartbeats"));
    assert_eq!(
        config.health.heartbeat_dir, expected,
        "health.heartbeat_dir should expand tilde in Some"
    );

    println!("✓ health.heartbeat_dir tilde expansion test passed");
}

/// Test that non-tilde paths in health.heartbeat_dir pass through unchanged.
#[tokio::test]
#[serial]
async fn health_heartbeat_dir_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
health:
  heartbeat_dir: /var/lib/needle/heartbeats
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/var/lib/needle/heartbeats"));
    assert_eq!(
        config.health.heartbeat_dir, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ health.heartbeat_dir non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// supervisor.heartbeat_path tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in supervisor.heartbeat_path configuration field.
#[tokio::test]
#[serial]
async fn supervisor_heartbeat_path_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
supervisor:
  heartbeat_path: ~/supervisor-heartbeat.sock
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("supervisor-heartbeat.sock"));
    assert_eq!(
        config.supervisor.heartbeat_path, expected,
        "supervisor.heartbeat_path should expand tilde in Some"
    );

    println!("✓ supervisor.heartbeat_path tilde expansion test passed");
}

/// Test that non-tilde paths in supervisor.heartbeat_path pass through unchanged.
#[tokio::test]
#[serial]
async fn supervisor_heartbeat_path_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
supervisor:
  heartbeat_path: /run/needle/supervisor-heartbeat.sock
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/run/needle/supervisor-heartbeat.sock"));
    assert_eq!(
        config.supervisor.heartbeat_path, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ supervisor.heartbeat_path non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// supervisor.socket_path tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in supervisor.socket_path configuration field.
#[tokio::test]
#[serial]
async fn supervisor_socket_path_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
supervisor:
  socket_path: ~/supervisor.sock
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("supervisor.sock"));
    assert_eq!(
        config.supervisor.socket_path, expected,
        "supervisor.socket_path should expand tilde in Some"
    );

    println!("✓ supervisor.socket_path tilde expansion test passed");
}

/// Test that non-tilde paths in supervisor.socket_path pass through unchanged.
#[tokio::test]
#[serial]
async fn supervisor_socket_path_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
supervisor:
  socket_path: /run/needle/supervisor.sock
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/run/needle/supervisor.sock"));
    assert_eq!(
        config.supervisor.socket_path, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ supervisor.socket_path non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// prompt.context_files tilde expansion tests (Vec<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in prompt.context_files configuration field.
#[tokio::test]
#[serial]
async fn prompt_context_files_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: multiple tilde paths in vector
    let yaml = r#"
prompt:
  context_files:
    - ~/context/notes.md
    - ~/context/references.txt
    - ~/nested/deep/context.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("context/notes.md"),
        PathBuf::from(&isolated_home).join("context/references.txt"),
        PathBuf::from(&isolated_home).join("nested/deep/context.md"),
    ];

    assert_eq!(
        config.prompt.context_files, expected,
        "prompt.context_files should expand all tilde paths in vector"
    );

    println!("✓ prompt.context_files tilde expansion test passed");
}

/// Test that non-tilde paths in prompt.context_files pass through unchanged.
#[tokio::test]
#[serial]
async fn prompt_context_files_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: mixed absolute and relative paths
    let yaml = r#"
prompt:
  context_files:
    - /absolute/notes.md
    - relative/references.txt
    - /opt/context/guide.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from("/absolute/notes.md"),
        PathBuf::from("relative/references.txt"),
        PathBuf::from("/opt/context/guide.md"),
    ];

    assert_eq!(
        config.prompt.context_files, expected,
        "non-tilde paths should pass through unchanged"
    );

    println!("✓ prompt.context_files non-tilde paths pass through unchanged");
}

/// Test prompt.context_files with mixed tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn prompt_context_files_mixed_tilde_non_tilde_paths() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: mixed tilde and absolute paths
    let yaml = r#"
prompt:
  context_files:
    - ~/notes.md
    - /absolute/references.txt
    - ~/context/guide.md
    - relative/readme.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = vec![
        PathBuf::from(&isolated_home).join("notes.md"),
        PathBuf::from("/absolute/references.txt"),
        PathBuf::from(&isolated_home).join("context/guide.md"),
        PathBuf::from("relative/readme.md"),
    ];

    assert_eq!(
        config.prompt.context_files, expected,
        "mixed tilde/non-tilde paths should be handled correctly"
    );

    println!("✓ prompt.context_files mixed tilde/non-tilde paths test passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// self_modification.canary_workspace tilde expansion tests (PathBuf)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in self_modification.canary_workspace configuration field.
#[tokio::test]
#[serial]
async fn self_modification_canary_workspace_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: ~/canary/workspace
    let yaml = r#"
self_modification:
  canary_workspace: ~/canary/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = PathBuf::from(&isolated_home).join("canary/workspace");
    assert_eq!(
        config.self_modification.canary_workspace, expected,
        "self_modification.canary_workspace should expand tilde correctly"
    );

    println!("✓ self_modification.canary_workspace tilde expansion test passed");
}

/// Test that non-tilde paths in self_modification.canary_workspace pass through unchanged.
#[tokio::test]
#[serial]
async fn self_modification_canary_workspace_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test 1: Absolute path without tilde
    let yaml = r#"
self_modification:
  canary_workspace: /opt/canary/workspace
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.self_modification.canary_workspace,
        PathBuf::from("/opt/canary/workspace"),
        "absolute path should pass through unchanged"
    );

    // Test 2: Relative path without tilde
    let yaml = r#"
self_modification:
  canary_workspace: relative/canary
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.self_modification.canary_workspace,
        PathBuf::from("relative/canary"),
        "relative path should pass through unchanged"
    );

    println!("✓ self_modification.canary_workspace non-tilde paths pass through unchanged");
}

/// Test self_modification.canary_workspace with bare tilde.
#[tokio::test]
#[serial]
async fn self_modification_canary_workspace_tilde_expansion_bare_tilde() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: bare tilde (~ alone)
    let yaml = r#"
self_modification:
  canary_workspace: ~
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert_eq!(
        config.self_modification.canary_workspace,
        PathBuf::from(&isolated_home),
        "bare tilde should expand to home directory"
    );

    println!("✓ self_modification.canary_workspace bare tilde expansion passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// prompt.variants[].content_file tilde expansion tests
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in prompt.variants[].content_file configuration field.
#[tokio::test]
#[serial]
async fn prompt_variants_content_file_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: variants with tilde paths
    let yaml = r#"
prompt:
  variants:
    default:
      - name: control
        weight: 100
        content_file: ~/prompts/default.txt
    detailed:
      - name: detailed
        weight: 100
        content_file: ~/prompts/detailed.md
    concise:
      - name: concise
        weight: 100
        content_file: ~/prompts/concise.txt
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify all variants expanded correctly
    let default_variants = config.prompt.variants.get("default").unwrap();
    assert_eq!(
        default_variants[0].content_file,
        PathBuf::from(&isolated_home).join("prompts/default.txt"),
        "default variant content_file should be expanded"
    );

    let detailed_variants = config.prompt.variants.get("detailed").unwrap();
    assert_eq!(
        detailed_variants[0].content_file,
        PathBuf::from(&isolated_home).join("prompts/detailed.md"),
        "detailed variant content_file should be expanded"
    );

    let concise_variants = config.prompt.variants.get("concise").unwrap();
    assert_eq!(
        concise_variants[0].content_file,
        PathBuf::from(&isolated_home).join("prompts/concise.txt"),
        "concise variant content_file should be expanded"
    );

    println!("✓ prompt.variants content_file tilde expansion test passed");
}

/// Test that non-tilde paths in prompt.variants[].content_file pass through unchanged.
#[tokio::test]
#[serial]
async fn prompt_variants_content_file_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: variants with absolute paths
    let yaml = r#"
prompt:
  variants:
    default:
      - name: default
        weight: 100
        content_file: /etc/needle/prompts/default.txt
    detailed:
      - name: detailed
        weight: 100
        content_file: relative/prompts/detailed.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify paths passed through unchanged
    let default_variants = config.prompt.variants.get("default").unwrap();
    assert_eq!(
        default_variants[0].content_file,
        PathBuf::from("/etc/needle/prompts/default.txt"),
        "absolute path should pass through unchanged"
    );

    let detailed_variants = config.prompt.variants.get("detailed").unwrap();
    assert_eq!(
        detailed_variants[0].content_file,
        PathBuf::from("relative/prompts/detailed.md"),
        "relative path should pass through unchanged"
    );

    println!("✓ prompt.variants content_file non-tilde paths pass through unchanged");
}

/// Test prompt.variants[].content_file with mixed tilde and non-tilde paths.
#[tokio::test]
#[serial]
async fn prompt_variants_content_file_mixed_tilde_non_tilde_paths() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: mixed tilde and absolute paths
    let yaml = r#"
prompt:
  variants:
    default:
      - name: tilde_variant
        weight: 50
        content_file: ~/prompts/default.txt
      - name: absolute_variant
        weight: 25
        content_file: /etc/needle/system.txt
      - name: relative_variant
        weight: 25
        content_file: relative/local.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify mixed paths handled correctly
    let default_variants = config.prompt.variants.get("default").unwrap();
    assert_eq!(
        default_variants[0].content_file,
        PathBuf::from(&isolated_home).join("prompts/default.txt"),
        "tilde path should be expanded"
    );
    assert_eq!(
        default_variants[1].content_file,
        PathBuf::from("/etc/needle/system.txt"),
        "absolute path should pass through unchanged"
    );
    assert_eq!(
        default_variants[2].content_file,
        PathBuf::from("relative/local.md"),
        "relative path should pass through unchanged"
    );

    println!("✓ prompt.variants content_file mixed tilde/non-tilde paths test passed");
}

// ──────────────────────────────────────────────────────────────────────────────
// strands.resolve.custom_template_path tilde expansion tests (Option<PathBuf>)
// ──────────────────────────────────────────────────────────────────────────────

/// Test tilde expansion in strands.resolve.custom_template_path configuration field.
#[tokio::test]
#[serial]
async fn resolve_custom_template_path_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: Some with tilde path
    let yaml = r#"
strands:
  resolve:
    custom_template_path: ~/custom-resolve-template.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from(&isolated_home).join("custom-resolve-template.md"));
    assert_eq!(
        config.strands.resolve.custom_template_path, expected,
        "strands.resolve.custom_template_path should expand tilde in Some"
    );

    println!("✓ strands.resolve.custom_template_path tilde expansion test passed");
}

/// Test that None in strands.resolve.custom_template_path remains None after expansion.
#[tokio::test]
#[serial]
async fn resolve_custom_template_path_none_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: None (field not set)
    let yaml = r#"
strands:
  resolve: {}
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    assert!(
        config.strands.resolve.custom_template_path.is_none(),
        "None should remain None after expansion"
    );

    println!("✓ strands.resolve.custom_template_path None remains None");
}

/// Test that non-tilde paths in strands.resolve.custom_template_path pass through unchanged.
#[tokio::test]
#[serial]
async fn resolve_custom_template_path_non_tilde_paths_unchanged() {
    let _guard = HomeGuard::isolate();

    // Test: absolute path
    let yaml = r#"
strands:
  resolve:
    custom_template_path: /etc/needle/resolve-template.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    let expected = Some(PathBuf::from("/etc/needle/resolve-template.md"));
    assert_eq!(
        config.strands.resolve.custom_template_path, expected,
        "absolute path should pass through unchanged"
    );

    println!("✓ strands.resolve.custom_template_path non-tilde paths pass through unchanged");
}

// ──────────────────────────────────────────────────────────────────────────────
// Combined tests - multiple config sections
// ──────────────────────────────────────────────────────────────────────────────

/// Test that multiple config sections can use tilde expansion simultaneously.
#[tokio::test]
#[serial]
async fn multiple_config_sections_tilde_expansion() {
    let _guard = HomeGuard::isolate();
    let isolated_home = env::var("HOME").unwrap();

    // Test: multiple config sections with tilde paths
    let yaml = r#"
worker:
  worker_binary_path: ~/bin/needle
agent:
  adapters_dir: ~/adapters
bead_cli:
  backend: bead-rs
  path: ~/.local/bin/bead
post_push_ci:
  state_dir: ~/.needle/ci-state
telemetry:
  file_sink:
    log_dir: ~/logs/needle
health:
  heartbeat_dir: ~/.needle/state/heartbeats
supervisor:
  heartbeat_path: ~/supervisor-heartbeat.sock
  socket_path: ~/supervisor.sock
prompt:
  context_files:
    - ~/context/notes.md
self_modification:
  canary_workspace: ~/canary/workspace
strands:
  resolve:
    custom_template_path: ~/custom-resolve-template.md
"#;

    let mut config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
    config.expand_tildes();

    // Verify all sections expanded correctly
    assert_eq!(
        config.worker.worker_binary_path,
        Some(PathBuf::from(&isolated_home).join("bin/needle")),
        "worker section should be expanded"
    );

    assert_eq!(
        config.agent.adapters_dir,
        PathBuf::from(&isolated_home).join("adapters"),
        "agent section should be expanded"
    );

    assert_eq!(
        config.bead_cli.path,
        Some(PathBuf::from(&isolated_home).join(".local/bin/bead")),
        "bead_cli section should be expanded"
    );

    assert_eq!(
        config.post_push_ci.state_dir,
        Some(PathBuf::from(&isolated_home).join(".needle/ci-state")),
        "post_push_ci section should be expanded"
    );

    assert_eq!(
        config.telemetry.file_sink.log_dir,
        Some(PathBuf::from(&isolated_home).join("logs/needle")),
        "telemetry section should be expanded"
    );

    assert_eq!(
        config.health.heartbeat_dir,
        Some(PathBuf::from(&isolated_home).join(".needle/state/heartbeats")),
        "health section should be expanded"
    );

    assert_eq!(
        config.supervisor.heartbeat_path,
        Some(PathBuf::from(&isolated_home).join("supervisor-heartbeat.sock")),
        "supervisor.heartbeat_path should be expanded"
    );

    assert_eq!(
        config.supervisor.socket_path,
        Some(PathBuf::from(&isolated_home).join("supervisor.sock")),
        "supervisor.socket_path should be expanded"
    );

    assert_eq!(
        config.prompt.context_files,
        vec![PathBuf::from(&isolated_home).join("context/notes.md")],
        "prompt section should be expanded"
    );

    assert_eq!(
        config.self_modification.canary_workspace,
        PathBuf::from(&isolated_home).join("canary/workspace"),
        "self_modification section should be expanded"
    );

    assert_eq!(
        config.strands.resolve.custom_template_path,
        Some(PathBuf::from(&isolated_home).join("custom-resolve-template.md")),
        "strands.resolve section should be expanded"
    );

    println!("✓ multiple config sections tilde expansion test passed");
}
