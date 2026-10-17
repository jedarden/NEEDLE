use std::fs;
use std::process::Command;

use needle::worker_env::apply_to_command_from_path;
use tempfile::tempdir;

#[test]
fn spawned_worker_process_receives_operator_environment() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("env");
    fs::write(&path, "PROBE_VALUE=harmless\n").unwrap();

    let mut command = Command::new("sh");
    command.args(["-c", "test \"$PROBE_VALUE\" = harmless"]);
    apply_to_command_from_path(&mut command, &path);

    assert!(command.status().unwrap().success());
}
