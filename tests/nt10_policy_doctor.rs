use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn needle_binary() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .unwrap_or_else(|| std::ffi::OsString::from(env!("CARGO_BIN_EXE_needle")))
}

struct Fixture {
    root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("isolated policy fixture");
        let home = root.path().join("home");
        let repository = root.path().join("repository");
        let workspace = repository.join("nested");
        fs::create_dir_all(home.join(".config/needle")).expect("create isolated config directory");
        fs::create_dir_all(&workspace).expect("create nested workspace");

        fs::write(
            home.join(".config/needle/config.yaml"),
            "agent:\n  default: codex\nprompt:\n  instructions: global instructions\n  templates:\n    pluck: custom pluck template\n",
        )
        .expect("write global policy fixture");
        fs::write(
            workspace.join(".needle.yaml"),
            "bead_cli:\n  backend: bead-rs\nprompt:\n  context_files:\n    - AGENTS.md\n",
        )
        .expect("write workspace policy fixture");
        fs::write(repository.join("AGENTS.md"), "repository instructions\n")
            .expect("write repository instructions");
        fs::write(
            workspace.join("AGENTS.md"),
            "nested repository instructions\n",
        )
        .expect("write nested instructions");
        fs::write(workspace.join("CLAUDE.md"), "adapter projection\n")
            .expect("write adapter instructions");

        Self {
            root,
            home,
            workspace,
        }
    }

    fn command(&self) -> Command {
        let state = self.root.path().join("state");
        let mut command = Command::new(needle_binary());
        command
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("NEEDLE_STATE_DIR", &state)
            .env_remove("NEEDLE_TEST_HARNESS")
            .env_remove("NEEDLE_INNER");
        command
    }

    fn run_json(&self) -> Output {
        self.command()
            .args([
                "doctor",
                "policy",
                "--workspace",
                self.workspace.to_str().expect("workspace is UTF-8"),
                "--adapter",
                "codex",
                "--json",
            ])
            .output()
            .expect("run policy doctor")
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "policy doctor stdout should be JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn provenance_path<'a>(document: &'a Value, suffix: &str) -> &'a Value {
    document["provenance"]
        .as_array()
        .expect("provenance array")
        .iter()
        .find(|source| {
            source["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(suffix))
        })
        .unwrap_or_else(|| panic!("provenance should contain {suffix}: {document}"))
}

#[test]
fn policy_doctor_reports_scoped_provenance_and_content_addressed_manifest() {
    let fixture = Fixture::new();
    let first = fixture.run_json();
    assert_eq!(
        first.status.code(),
        Some(0),
        "policy doctor should pass:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_document = json(&first);
    assert_eq!(first_document["status"], "pass");
    assert_eq!(first_document["conflicts"], serde_json::json!([]));
    assert_eq!(first_document["manifest"]["version"], "effective-policy-v1");
    assert_eq!(
        first_document["manifest"]["hash"].as_str().map(str::len),
        Some(64)
    );

    for suffix in ["config.yaml", ".needle.yaml", "AGENTS.md", "CLAUDE.md"] {
        let source = provenance_path(&first_document, suffix);
        assert_eq!(
            source["content_sha256"].as_str().map(str::len),
            Some(64),
            "source {suffix} should carry a content digest"
        );
        assert!(
            source["authority"].is_string(),
            "source {suffix} should carry authority provenance"
        );
        assert!(
            source["scope"].is_string(),
            "source {suffix} should carry scope provenance"
        );
    }

    let second = fixture.run_json();
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        first.stdout, second.stdout,
        "policy report should be deterministic for unchanged inputs"
    );

    let human = fixture
        .command()
        .args([
            "doctor",
            "policy",
            "--workspace",
            fixture.workspace.to_str().expect("workspace is UTF-8"),
            "--adapter",
            "codex",
        ])
        .output()
        .expect("run human policy doctor");
    assert_eq!(human.status.code(), Some(0));
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(human_stdout.contains("Provenance:"));
    assert!(human_stdout.contains("Manifest:"));
    assert!(human_stdout.contains("Conflicts: none"));
}

#[test]
fn policy_doctor_reports_deterministic_equal_authority_conflict() {
    let fixture = Fixture::new();
    fs::write(
        fixture.workspace.join("AGENTS.local.md"),
        "contradictory nested instructions\n",
    )
    .expect("write conflicting instruction source");

    let first = fixture.run_json();
    assert_eq!(
        first.status.code(),
        Some(1),
        "conflicting policy must fail closed:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_document = json(&first);
    assert_eq!(first_document["status"], "fail");
    assert_eq!(first_document["manifest"], Value::Null);
    assert_eq!(
        first_document["conflicts"].as_array().map(Vec::len),
        Some(1)
    );
    let conflict = &first_document["conflicts"][0];
    assert_eq!(conflict["authority"], "repository");
    assert_eq!(conflict["source_ids"].as_array().map(Vec::len), Some(2));
    assert!(
        conflict["source_ids"][0]
            .as_str()
            .expect("first conflict source")
            < conflict["source_ids"][1]
                .as_str()
                .expect("second conflict source"),
        "conflict source order should be stable"
    );

    let second = fixture.run_json();
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(
        first.stdout, second.stdout,
        "conflict report should be deterministic"
    );
}
