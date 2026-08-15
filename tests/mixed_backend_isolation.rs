//! Mixed-backend routing gate with both real CLIs installed on the same PATH.
//!
//! Run with:
//! `BEAD_RS_BIN=/path/to/bead BF_BIN=/path/to/bf cargo test --test mixed_backend_isolation -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn in_workspace(binary: &Path, workspace: &Path) -> Command {
    let mut command = Command::new(binary);
    command.current_dir(workspace);
    command
}

#[cfg(unix)]
fn write_wrapper(path: &Path, target: &Path, log: &Path, marker: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' '{marker}' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            target.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
#[cfg(unix)]
#[ignore = "release gate requiring real bead-rs and bead-forge binaries"]
fn each_workspace_invokes_only_its_explicit_backend() {
    let bead = std::env::var_os("BEAD_RS_BIN")
        .map(PathBuf::from)
        .expect("BEAD_RS_BIN must name a real bead-rs binary");
    let bf = std::env::var_os("BF_BIN")
        .map(PathBuf::from)
        .expect("BF_BIN must name a real bead-forge binary");
    assert!(bead.is_file());
    assert!(bf.is_file());

    let root = tempfile::tempdir().unwrap();
    let bead_workspace = root.path().join("bead-workspace");
    let bf_workspace = root.path().join("bf-workspace");
    let home = root.path().join("home");
    let bin = root.path().join("bin");
    fs::create_dir_all(&bead_workspace).unwrap();
    fs::create_dir_all(&bf_workspace).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();

    run(in_workspace(&bead, &bead_workspace).args(["init", "--prefix", "mixbead"]));
    run(in_workspace(&bead, &bead_workspace).args([
        "create",
        "--title",
        "bead-rs routing fixture",
    ]));
    run(in_workspace(&bead, &bead_workspace).args(["sync", "flush-only"]));

    run(in_workspace(&bf, &bf_workspace).args(["init", "--prefix", "mixbf"]));
    run(in_workspace(&bf, &bf_workspace).args(["create", "--title", "bead-forge routing fixture"]));
    run(in_workspace(&bf, &bf_workspace).args(["sync", "--flush-only"]));

    fs::write(
        bead_workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-rs\n",
    )
    .unwrap();
    fs::write(
        bf_workspace.join(".needle.yaml"),
        "bead_cli:\n  backend: bead-forge\n",
    )
    .unwrap();

    let invocation_log = root.path().join("invocations.log");
    write_wrapper(&bin.join("bead"), &bead, &invocation_log, "bead");
    write_wrapper(&bin.join("bf"), &bf, &invocation_log, "bf");
    let path =
        std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let needle = PathBuf::from(env!("CARGO_BIN_EXE_needle"));

    let bead_output = run(Command::new(&needle)
        .args(["doctor", "--workspace", bead_workspace.to_str().unwrap()])
        .env("HOME", &home)
        .env("PATH", &path));
    assert!(String::from_utf8_lossy(&bead_output.stdout).contains("[PASS]  Bead store"));
    let bead_invocations = fs::read_to_string(&invocation_log).unwrap();
    assert!(bead_invocations
        .lines()
        .any(|line| line.starts_with("bead ")));
    assert!(!bead_invocations.lines().any(|line| line.starts_with("bf ")));

    fs::write(&invocation_log, "").unwrap();
    let bf_output = run(Command::new(&needle)
        .args(["doctor", "--workspace", bf_workspace.to_str().unwrap()])
        .env("HOME", &home)
        .env("PATH", &path));
    assert!(String::from_utf8_lossy(&bf_output.stdout).contains("[PASS]  Bead store"));
    let bf_invocations = fs::read_to_string(&invocation_log).unwrap();
    assert!(bf_invocations.lines().any(|line| line.starts_with("bf ")));
    assert!(!bf_invocations.lines().any(|line| line.starts_with("bead ")));

    for (name, yaml) in [("unbound", "")] {
        let workspace = root.path().join(name);
        fs::create_dir_all(workspace.join(".beads")).unwrap();
        if !yaml.is_empty() {
            fs::write(workspace.join(".needle.yaml"), yaml).unwrap();
        }
        fs::write(&invocation_log, "").unwrap();
        let output = run(Command::new(&needle)
            .args(["doctor", "--workspace", workspace.to_str().unwrap()])
            .env("HOME", &home)
            .env("PATH", &path));
        assert!(String::from_utf8_lossy(&output.stdout).contains("[FAIL]  Bead store"));
        assert_eq!(fs::read_to_string(&invocation_log).unwrap(), "");
    }

    let unknown = root.path().join("unknown");
    fs::create_dir_all(unknown.join(".beads")).unwrap();
    fs::write(
        unknown.join(".needle.yaml"),
        "bead_cli:\n  backend: does-not-exist\n",
    )
    .unwrap();
    fs::write(&invocation_log, "").unwrap();
    let output = Command::new(&needle)
        .args(["doctor", "--workspace", unknown.to_str().unwrap()])
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does-not-exist"));
    assert_eq!(fs::read_to_string(&invocation_log).unwrap(), "");

    let missing_path = root.path().join("missing-explicit-path");
    fs::create_dir_all(missing_path.join(".beads")).unwrap();
    fs::write(
        missing_path.join(".needle.yaml"),
        format!(
            "bead_cli:\n  backend: bead-rs\n  explicit_path: {}\n",
            root.path().join("does-not-exist").display()
        ),
    )
    .unwrap();
    fs::write(&invocation_log, "").unwrap();
    let output = run(Command::new(&needle)
        .args(["doctor", "--workspace", missing_path.to_str().unwrap()])
        .env("HOME", &home)
        .env("PATH", &path));
    assert!(String::from_utf8_lossy(&output.stdout).contains("does not exist"));
    assert_eq!(fs::read_to_string(&invocation_log).unwrap(), "");

    let mismatch = root.path().join("identity-mismatch");
    fs::create_dir_all(mismatch.join(".beads")).unwrap();
    fs::write(
        mismatch.join(".needle.yaml"),
        format!(
            "bead_cli:\n  backend: bead-rs\n  explicit_path: {}\n",
            bin.join("bf").display()
        ),
    )
    .unwrap();
    fs::write(&invocation_log, "").unwrap();
    let output = run(Command::new(&needle)
        .args(["doctor", "--workspace", mismatch.to_str().unwrap()])
        .env("HOME", &home)
        .env("PATH", &path));
    assert!(String::from_utf8_lossy(&output.stdout).contains("identity mismatch"));
    let mismatch_invocations = fs::read_to_string(&invocation_log).unwrap();
    assert_eq!(mismatch_invocations.lines().count(), 1);
    assert!(mismatch_invocations.starts_with("bf --version"));
}
