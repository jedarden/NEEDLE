//! A killed gate-health writer must leave the published JSON intact.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_MARKER: &str = "NEEDLE_GATE_HEALTH_ATOMIC_WRITE_CHILD";
const WORKSPACE_ENV: &str = "NEEDLE_GATE_HEALTH_ATOMIC_WRITE_WORKSPACE";

#[test]
fn writer_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    let workspace = PathBuf::from(std::env::var_os(WORKSPACE_ENV).expect("workspace fixture"));
    let mut state = needle::gate_health::load_state(&workspace)
        .expect("read old state")
        .expect("old state exists");
    state.last_reason = "new".repeat(16 * 1024 * 1024);
    loop {
        needle::gate_health::save_state(&state).expect("publish new state");
    }
}

fn partial_temporary_write_present(dir: &Path, expected_len: u64) -> bool {
    fs::read_dir(dir)
        .expect("read private gate-health directory")
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("gate-health-")
                && name.ends_with(".tmp")
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() > 0 && metadata.len() < expected_len)
        })
}

#[test]
fn kill_mid_write_preserves_complete_record() {
    let fixture = tempfile::tempdir().expect("create private state root");
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&workspace).expect("create fixture workspace");
    std::env::set_var("HOME", fixture.path());
    std::env::set_var("NEEDLE_STATE_DIR", fixture.path().join(".needle"));
    let (state, _) =
        needle::gate_health::record_error(&workspace, "gate".to_string(), "old".to_string())
            .expect("write old state");
    let old = state.expect("recorded old state");
    let path = needle::gate_health::state_file_path(&workspace).expect("state path");
    let old_bytes = fs::read(&path).expect("read old state bytes");
    let expected_new_len =
        old_bytes.len() as u64 - old.last_reason.len() as u64 + (3 * 16 * 1024 * 1024) as u64;
    let dir = path.parent().expect("state parent");

    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .arg("--exact")
        .arg("writer_child")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(WORKSPACE_ENV, &workspace)
        .env("HOME", fixture.path())
        .env("NEEDLE_STATE_DIR", fixture.path().join(".needle"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn writer process");
    let deadline = Instant::now() + Duration::from_secs(15);
    let observed = loop {
        if partial_temporary_write_present(dir, expected_new_len) {
            break true;
        }
        if child.try_wait().expect("inspect writer").is_some() || Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    child.kill().expect("kill writer during temporary write");
    child.wait().expect("reap killed writer");
    assert!(observed, "writer never reached a partial temporary write");

    let published = fs::read(&path).expect("published state survives kill");
    let parsed: needle::gate_health::GateHealthState =
        serde_json::from_slice(&published).expect("published record is complete JSON");
    assert_eq!(parsed.workspace, old.workspace);
    assert!(parsed.last_reason == "old" || parsed.last_reason.starts_with("new"));
    if parsed.last_reason == "old" {
        assert_eq!(published, old_bytes);
    } else {
        assert_eq!(parsed.last_reason.len(), 3 * 16 * 1024 * 1024);
    }
}
