//! Real NEEDLE lifecycle gate for the bead-rs backend.
//!
//! Run explicitly after installing or building bead-rs:
//! `BEAD_RS_BIN=/path/to/bead cargo test --test bead_rs_lifecycle -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn run(command: &mut Command) -> Output {
    let description = format!("{command:?}");
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn {description}: {error}"));
    assert!(
        output.status.success(),
        "{description} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(command: &mut Command) -> String {
    String::from_utf8(run(command).stdout)
        .expect("fixture commands must emit UTF-8")
        .trim()
        .to_string()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn bead_command(binary: &Path, workspace: &Path) -> Command {
    let mut command = Command::new(binary);
    command.current_dir(workspace);
    command
}

#[test]
#[ignore = "release gate requiring a real bead-rs binary; set BEAD_RS_BIN"]
fn needle_claims_closes_and_restores_a_bead_rs_workspace() {
    let source_binary = std::env::var_os("BEAD_RS_BIN")
        .map(PathBuf::from)
        .expect("BEAD_RS_BIN must name the pinned real bead-rs binary");
    assert!(source_binary.is_file(), "BEAD_RS_BIN does not exist");

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let restore = root.path().join("restore");
    let home = root.path().join("home");
    let bin_dir = root.path().join("bin");
    let adapters = home.join(".config/needle/adapters");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&restore).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&adapters).unwrap();

    let bead = bin_dir.join("bead");
    fs::copy(&source_binary, &bead).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&bead).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bead, permissions).unwrap();
    }
    assert!(!bin_dir.join("bf").exists());
    assert!(!bin_dir.join("br").exists());
    assert!(!Path::new("/usr/bin/bf").exists());
    assert!(!Path::new("/usr/bin/br").exists());
    assert!(!Path::new("/bin/bf").exists());
    assert!(!Path::new("/bin/br").exists());
    assert_eq!(
        stdout(bead_command(&bead, &workspace).arg("--version")),
        "bead 0.1.3"
    );

    run(bead_command(&bead, &workspace).args(["init", "--prefix", "e2e"]));
    let blocker = stdout(bead_command(&bead, &workspace).args([
        "create",
        "--title",
        "Lifecycle blocker",
        "--priority",
        "1",
    ]));
    let blocked = stdout(bead_command(&bead, &workspace).args([
        "create",
        "--title",
        "Lifecycle dependent",
        "--priority",
        "2",
    ]));
    run(bead_command(&bead, &workspace)
        .args(["dep", "add", &blocked, &blocker, "--kind", "blocks"]));

    let ready = stdout(bead_command(&bead, &workspace).args(["list", "--ready", "--json"]));
    assert!(ready.contains(&blocker));
    assert!(!ready.contains(&blocked));

    fs::write(
        workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();
    fs::write(
        home.join(".config/needle/config.yaml"),
        format!(
            "agent:\n  default: e2e-agent\n  timeout: 30\n  adapters_dir: {}\n  routing: null\nworker:\n  idle_action: exit\n  enforce_shipped_work: false\n  cpu_load_warn: 1.0\n  memory_free_warn_mb: 1\nworkspace:\n  home: {}\nstrands:\n  explore:\n    enabled: false\n    workspace_root: {}\n    workspaces: []\n  splice:\n    enabled: false\n  reflect:\n    enabled: false\ntelemetry:\n  file_sink:\n    enabled: true\n    log_dir: {}\n",
            adapters.display(),
            home.join(".needle").display(),
            root.path().display(),
            home.join("logs").display(),
        ),
    )
    .unwrap();
    fs::write(
        adapters.join("e2e-agent.yaml"),
        format!(
            "name: e2e-agent\ndescription: deterministic bead-rs gate\nagent_cli: /bin/true\ninvoke_template: \"cd {{workspace}} && {} close {{bead_id}} --reason 'closed by NEEDLE bead-rs gate'\"\ntimeout_secs: 30\nprovider: local\nmodel: e2e\n",
            bead.display()
        ),
    )
    .unwrap();

    let needle = PathBuf::from(env!("CARGO_BIN_EXE_needle"));
    let started = Instant::now();
    let output = run(Command::new(&needle)
        .args([
            "run",
            "--workspace",
            workspace.to_str().unwrap(),
            "--identifier",
            "bead-rs-e2e",
            "--hot-reload",
            "false",
        ])
        .env("HOME", &home)
        .env("NEEDLE_INNER", "1")
        .env(
            "PATH",
            std::env::join_paths([bin_dir.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                .unwrap(),
        ));
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "normal shutdown waited for a stale timeout task"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("stopped unexpectedly"));

    for id in [&blocker, &blocked] {
        let shown = stdout(bead_command(&bead, &workspace).args(["show", id, "--json"]));
        assert!(shown.contains("\"status\":\"closed\""), "{shown}");
    }
    let dependent = stdout(bead_command(&bead, &workspace).args(["show", &blocked, "--json"]));
    assert!(dependent.contains(&format!("\"blocker\":\"{blocker}\"")));

    run(bead_command(&bead, &workspace).args(["sync", "flush-only"]));
    run(bead_command(&bead, &workspace).arg("doctor"));

    fs::create_dir_all(restore.join(".beads")).unwrap();
    fs::copy(
        workspace.join(".beads/config.json"),
        restore.join(".beads/config.json"),
    )
    .unwrap();
    copy_tree(
        &workspace.join(".beads/checkpoint"),
        &restore.join(".beads/checkpoint"),
    );
    run(bead_command(&bead, &restore).arg("init"));
    run(bead_command(&bead, &restore).args([
        "sync",
        "import-only",
        "--input",
        ".beads/checkpoint",
        "--restore-into-empty",
        "--actor",
        "needle-e2e",
    ]));
    run(bead_command(&bead, &restore).arg("doctor"));
    let restored = stdout(bead_command(&bead, &restore).args(["show", &blocked, "--json"]));
    assert!(restored.contains("\"status\":\"closed\""));
    assert!(restored.contains(&format!("\"blocker\":\"{blocker}\"")));
}

#[test]
#[ignore = "release gate requiring a real bead-rs binary; set BEAD_RS_BIN"]
fn sync_flush_only_rejects_profile_flag() {
    let source_binary = std::env::var_os("BEAD_RS_BIN")
        .map(PathBuf::from)
        .expect("BEAD_RS_BIN must name the pinned real bead-rs binary");
    assert!(source_binary.is_file(), "BEAD_RS_BIN does not exist");

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let bin_dir = root.path().join("bin");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let bead = bin_dir.join("bead");
    fs::copy(&source_binary, &bead).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&bead).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bead, permissions).unwrap();
    }

    // Initialize a bead-rs workspace with isolated HOME and skip foreign workspace
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "init", "--prefix", "test"]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    // Test 1: --profile is rejected on default forensic path (no --output)
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--profile",
        "native-v1",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "flush-only with --profile should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--profile"),
        "stderr should reject --profile: {}",
        stderr
    );

    // Test 2: --profile is rejected when using --output
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--output",
        root.path().join("test.jsonl").to_str().unwrap(),
        "--profile",
        "native-v1",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "flush-only with --output and --profile should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--profile"),
        "stderr should reject --profile: {}",
        stderr
    );

    // Test 3: flush-only works correctly without --profile
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "sync", "flush-only"]);
    cmd.env("HOME", &home);
    run(&mut cmd);
}

#[test]
#[ignore = "release gate requiring a real bead-rs binary; set BEAD_RS_BIN"]
fn sync_import_only_diagnostics_flag() {
    let source_binary = std::env::var_os("BEAD_RS_BIN")
        .map(PathBuf::from)
        .expect("BEAD_RS_BIN must name the pinned real bead-rs binary");
    assert!(source_binary.is_file(), "BEAD_RS_BIN does not exist");

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let restore = root.path().join("restore");
    let home = root.path().join("home");
    let bin_dir = root.path().join("bin");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&restore).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let bead = bin_dir.join("bead");
    fs::copy(&source_binary, &bead).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&bead).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bead, permissions).unwrap();
    }

    // Initialize a bead-rs workspace with isolated HOME and skip foreign workspace
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "init", "--prefix", "test"]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    // Create some beads to have checkpoint data
    let _ = stdout(bead_command(&bead, &workspace).args([
        "create",
        "--title",
        "Diagnostics test blocker",
        "--priority",
        "1",
    ]));
    let _ = stdout(bead_command(&bead, &workspace).args([
        "create",
        "--title",
        "Diagnostics test dependent",
        "--priority",
        "2",
    ]));

    // Flush to create checkpoint data
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "sync", "flush-only"]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    // Test 1: --diagnostics works with dry-run (validation mode)
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "import-only",
        "--input",
        ".beads/checkpoint",
        "--diagnostics",
        "--dry-run",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    // In diagnostics mode with dry-run, this should succeed (validates without mutating)
    assert!(
        output.status.success(),
        "import-only with --diagnostics --dry-run should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should indicate it's a dry-run validation
    assert!(
        stdout.contains("dry-run") || stdout.contains("validation"),
        "stdout should indicate dry-run or validation: {}",
        stdout
    );

    // Test 2: --diagnostics works alone (simple import mode, no --restore-into-empty or --merge)
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "import-only",
        "--input",
        ".beads/checkpoint",
        "--diagnostics",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    // Should succeed since --diagnostics uses simple import mode
    assert!(
        output.status.success(),
        "import-only with --diagnostics should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("diagnostic") || stdout.contains("validation"),
        "stdout should indicate diagnostic mode: {}",
        stdout
    );

    // Test 3: --diagnostics is incompatible with --restore-into-empty
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "import-only",
        "--input",
        ".beads/checkpoint",
        "--restore-into-empty",
        "--actor",
        "test",
        "--diagnostics",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "import-only with --restore-into-empty and --diagnostics should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("incompatible") || stderr.contains("conflict") || stderr.contains("cannot"),
        "stderr should indicate incompatibility: {}",
        stderr
    );

    // Test 4: --diagnostics is incompatible with --merge
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "import-only",
        "--input",
        ".beads/checkpoint",
        "--merge",
        "--actor",
        "test",
        "--diagnostics",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "import-only with --merge and --diagnostics should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("incompatible") || stderr.contains("conflict") || stderr.contains("cannot"),
        "stderr should indicate incompatibility: {}",
        stderr
    );

    // Test 5: Diagnostics mode works on a fresh restore workspace
    fs::create_dir_all(restore.join(".beads")).unwrap();
    fs::write(
        restore.join(".beads/config.json"),
        fs::read_to_string(workspace.join(".beads/config.json")).unwrap(),
    )
    .unwrap();

    let mut cmd = bead_command(&bead, &restore);
    cmd.args([
        "--skip-foreign-workspace",
        "init",
        "--prefix",
        "restore-test",
    ]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    let mut cmd = bead_command(&bead, &restore);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "import-only",
        "--input",
        workspace.join(".beads/checkpoint").to_str().unwrap(),
        "--diagnostics",
        "--dry-run",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        output.status.success(),
        "import-only with --diagnostics --dry-run on restore workspace should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "release gate requiring a real bead-rs binary; set BEAD_RS_BIN"]
fn sync_flush_only_profile_flag_comprehensive() {
    let source_binary = std::env::var_os("BEAD_RS_BIN")
        .map(PathBuf::from)
        .expect("BEAD_RS_BIN must name the pinned real bead-rs binary");
    assert!(source_binary.is_file(), "BEAD_RS_BIN does not exist");

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let bin_dir = root.path().join("bin");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let bead = bin_dir.join("bead");
    fs::copy(&source_binary, &bead).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&bead).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bead, permissions).unwrap();
    }

    // Initialize a bead-rs workspace with isolated HOME and skip foreign workspace
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "init", "--prefix", "test"]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    // Test 1: --profile is rejected on default forensic path (no --output)
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--profile",
        "native-v1",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "flush-only with --profile should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--profile"),
        "stderr should reject --profile: {}",
        stderr
    );

    // Test 2: --profile is rejected when using --output
    let output_path = root.path().join("test-output.jsonl");
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--output",
        output_path.to_str().unwrap(),
        "--profile",
        "native-v1",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "flush-only with --output and --profile should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--profile"),
        "stderr should reject --profile: {}",
        stderr
    );

    // Test 3: flush-only works correctly with --output alone (no --profile)
    let output_path = root.path().join("working-output.jsonl");
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--output",
        output_path.to_str().unwrap(),
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        output.status.success(),
        "flush-only with --output (without --profile) should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Verify the output file was created
    assert!(
        output_path.exists(),
        "output file should be created with --output"
    );

    // Test 4: flush-only works correctly with default path (no --output, no --profile)
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args(["--skip-foreign-workspace", "sync", "flush-only"]);
    cmd.env("HOME", &home);
    run(&mut cmd);

    // Test 5: Multiple --profile flags are rejected
    let mut cmd = bead_command(&bead, &workspace);
    cmd.args([
        "--skip-foreign-workspace",
        "sync",
        "flush-only",
        "--profile",
        "native-v1",
        "--profile",
        "native-v1",
    ]);
    cmd.env("HOME", &home);
    let output = cmd.output().expect("failed to execute bead command");
    assert!(
        !output.status.success(),
        "flush-only with multiple --profile flags should fail"
    );
}
