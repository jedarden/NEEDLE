//! Operator-facing dedupe contracts against isolated bead-rs workspaces.
//! Every NEEDLE subprocess receives a temporary HOME and explicit Explore root.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn needle_binary() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_needle")))
}

// Older bead-rs releases follow parent directories during `init` and can
// silently adopt a real store when TMPDIR sits below a repository. A config
// marker without a database makes the requested directory the init target.
fn mark_native_workspace(path: &Path) -> Result<()> {
    fs::create_dir_all(path.join(".beads"))?;
    fs::write(path.join(".beads/config.json"), "{}")?;
    Ok(())
}

// A queue-fence wrapper on PATH can redirect a command to an operator store.
fn native_bead_binary() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("BEAD_RS_BIN") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(paths) = which::which_all("bead") {
        candidates.extend(paths);
    }
    candidates.push(PathBuf::from("/home/coding/.cargo/bin/bead"));
    let mut seen = HashSet::new();
    for candidate in candidates {
        let identity = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if !candidate.is_file() || !seen.insert(identity) {
            continue;
        }
        let Ok(probe) = tempfile::tempdir() else {
            continue;
        };
        let workspace = probe.path().join("workspace");
        let home = probe.path().join("home");
        if fs::create_dir_all(&workspace).is_err()
            || fs::create_dir_all(&home).is_err()
            || mark_native_workspace(&workspace).is_err()
        {
            continue;
        }
        let usable = Command::new(&candidate)
            .current_dir(&workspace)
            .env("HOME", &home)
            .args(["init", "--prefix", "probe", "--skip-foreign-workspace"])
            .output()
            .is_ok_and(|output| output.status.success());
        if usable && workspace.join(".beads/beads.db").is_file() {
            return candidate;
        }
    }
    panic!("native bead-rs CLI required for proposal_backlog_dedupe")
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    bead: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("needle-proposal-dedupe-")
            .tempdir()?;
        let root = temp.path().join("workspaces");
        let home = temp.path().join("home");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(home.join(".config/needle"))?;
        fs::write(home.join(".config/needle/config.yaml"), format!(
            "strands:\n  explore:\n    enabled: false\n    workspace_root: {}\n  weave:\n    enabled: false\n    duplicate_similarity_threshold: 0.5\ntelemetry:\n  file_sink:\n    enabled: false\n", root.display()))?;
        Ok(Self {
            _temp: temp,
            root,
            home,
            bead: native_bead_binary(),
        })
    }
    fn workspace(&self, name: &str) -> Result<PathBuf> {
        let path = self.root.join(name);
        fs::create_dir_all(&path)?;
        mark_native_workspace(&path)?;
        self.bead(&path, &["init", "--prefix", name])?;
        if !path.join(".beads/beads.db").is_file() {
            bail!(
                "bead init did not create an isolated store in {}",
                path.display()
            );
        }
        fs::write(path.join(".needle.yaml"), format!(
            "bead_cli:\n  backend: bead-rs\n  path: {}\nstrands:\n  weave:\n    duplicate_similarity_threshold: 0.5\n", self.bead.display()))?;
        Ok(path)
    }
    fn bf_workspace(&self, name: &str) -> Result<PathBuf> {
        let path = self.root.join(name);
        fs::create_dir_all(path.join(".beads"))?;
        fs::write(path.join(".beads/config.yaml"), "backend: bf\n")?;
        fs::write(path.join(".needle.yaml"), "bead_cli:\n  backend: bf\n")?;
        Ok(path)
    }
    fn command(&self, dir: &Path, program: &Path, args: &[&str]) -> Result<Output> {
        Command::new(program)
            .current_dir(dir)
            .env("HOME", &self.home)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {:?} {:?}", program, args))
    }
    fn bead(&self, dir: &Path, args: &[&str]) -> Result<Output> {
        let output = self.command(dir, &self.bead, args)?;
        if !output.status.success() {
            bail!(
                "bead {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(output)
    }
    fn create(&self, dir: &Path, title: &str, body: &str, held: bool) -> Result<String> {
        let mut args = vec!["create", "--title", title, "--description", body];
        if held {
            args.extend(["--label", "proposal-pending", "--label", "human"]);
        }
        let id = String::from_utf8(self.bead(dir, &args)?.stdout)?
            .trim()
            .to_string();
        if id.is_empty() {
            bail!("bead create returned no ID");
        }
        Ok(id)
    }
    fn show(&self, dir: &Path, id: &str) -> Result<Value> {
        let output: Value =
            serde_json::from_slice(&self.bead(dir, &["show", id, "--json"])?.stdout)
                .context("bead show JSON")?;
        match output {
            Value::Array(mut rows) if rows.len() == 1 => Ok(rows.remove(0)),
            Value::Object(_) => Ok(output),
            _ => bail!("bead show did not return one object"),
        }
    }
    fn needle(&self, args: &[&str]) -> Result<Output> {
        self.command(&self.root, &needle_binary(), args)
    }
    fn report(&self) -> Result<Value> {
        let output = self.needle(&[
            "proposals",
            "dedupe",
            "--root",
            self.root.to_str().context("root UTF-8")?,
            "--json",
        ])?;
        if !output.status.success() {
            bail!("dedupe: {}", String::from_utf8_lossy(&output.stderr));
        }
        serde_json::from_slice(&output.stdout).context("dedupe JSON")
    }
    fn apply(&self, id: &str) -> Result<Output> {
        self.needle(&[
            "proposals",
            "dedupe",
            "--apply",
            "--group",
            id,
            "--root",
            self.root.to_str().context("root UTF-8")?,
            "--json",
        ])
    }
}

fn groups<'a>(report: &'a Value, tier: &str) -> &'a [Value] {
    report[tier].as_array().expect("tier array")
}
fn ids(group: &Value) -> HashSet<&str> {
    group["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|member| member["id"].as_str().expect("member id"))
        .collect()
}
fn evidence(paths: &[&str]) -> String {
    let mut body = "\n## Evidence at HEAD".to_string();
    for path in paths {
        body.push_str(&format!("\n- `{path}:1`: `observed defect`"));
    }
    body
}
fn checkpoint_files(path: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                out.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(&path)?);
            }
        }
        Ok(())
    }
    let root = path.join(".beads/checkpoint");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out)?;
    Ok(out)
}

#[test]
fn high_tier_survivor_and_report_read_only() -> Result<()> {
    let f = Fixture::new()?;
    let w = f.workspace("high")?;
    fs::create_dir_all(w.join("src"))?;
    fs::write(w.join("src/session.rs"), "observed defect\n")?;
    fs::write(w.join("src/extra.rs"), "observed defect\n")?;
    let old = f.create(
        &w,
        "Repair session token refresh handler",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let new = f.create(
        &w,
        "Repair session token refresh handler",
        &evidence(&["src/session.rs", "src/extra.rs"]),
        true,
    )?;
    let before = checkpoint_files(&w)?;
    let old_rev = f.show(&w, &old)?["revision"].clone();
    let new_rev = f.show(&w, &new)?["revision"].clone();
    let report = f.report()?;
    let repeated = f.report()?;
    assert_eq!(groups(&report, "high_groups").len(), 1);
    assert!(groups(&report, "low_groups").is_empty());
    let group = &groups(&report, "high_groups")[0];
    assert_eq!(group["id"], repeated["high_groups"][0]["id"]);
    assert_eq!(group["survivor"], new);
    assert_eq!(ids(group), HashSet::from([old.as_str(), new.as_str()]));
    assert!(group["members"]
        .as_array()
        .expect("members")
        .iter()
        .all(|member| member["shared_artifacts"]
            .as_array()
            .is_some_and(|paths| paths.iter().any(|path| path == "src/session.rs"))));
    assert_eq!(checkpoint_files(&w)?, before);
    assert_eq!(f.show(&w, &old)?["revision"], old_rev);
    assert_eq!(f.show(&w, &new)?["revision"], new_rev);
    Ok(())
}

#[test]
fn low_tier_distinct_gap_and_unheld_filter() -> Result<()> {
    let f = Fixture::new()?;
    let w = f.workspace("low")?;
    let a = f.create(
        &w,
        "Repair session token refresh handler",
        &evidence(&["src/first.rs"]),
        true,
    )?;
    let b = f.create(
        &w,
        "Repair session token refresh handler",
        &evidence(&["src/second.rs"]),
        true,
    )?;
    let distinct = f.create(
        &w,
        "Document glacier survey navigation maps",
        &evidence(&["docs/glaciers.md"]),
        true,
    )?;
    let unheld = f.create(
        &w,
        "Repair session token refresh handler",
        &evidence(&["src/first.rs"]),
        false,
    )?;
    let report = f.report()?;
    assert!(groups(&report, "high_groups").is_empty());
    assert_eq!(groups(&report, "low_groups").len(), 1);
    let group = &groups(&report, "low_groups")[0];
    assert_eq!(ids(group), HashSet::from([a.as_str(), b.as_str()]));
    assert!(!ids(group).contains(distinct.as_str()));
    assert!(!ids(group).contains(unheld.as_str()));
    assert_eq!(group["survivor"], a);
    Ok(())
}

#[test]
fn chaining_never_groups_nonmatching_endpoints() -> Result<()> {
    let f = Fixture::new()?;
    let w = f.workspace("chain")?;
    // A-B and B-C have Jaccard 0.5; A-C have zero.
    let a = f.create(&w, "alpha beta", "", true)?;
    let b = f.create(&w, "alpha beta gamma delta", "", true)?;
    let c = f.create(&w, "gamma delta", "", true)?;
    let report = f.report()?;
    for group in groups(&report, "high_groups")
        .iter()
        .chain(groups(&report, "low_groups"))
    {
        assert!(
            !(ids(group).contains(a.as_str()) && ids(group).contains(c.as_str())),
            "{group}"
        );
    }
    assert_eq!(groups(&report, "low_groups").len(), 1);
    let group = &groups(&report, "low_groups")[0];
    assert_eq!(group["survivor"], a);
    assert_eq!(ids(group), HashSet::from([a.as_str(), b.as_str()]));
    assert!(!ids(group).contains(c.as_str()));
    Ok(())
}

#[test]
fn no_cross_workspace_group_and_bf_skip() -> Result<()> {
    let f = Fixture::new()?;
    let first = f.workspace("first")?;
    let second = f.workspace("second")?;
    let legacy = f.bf_workspace("legacy")?;
    f.create(&first, "Repair session token refresh", "Same gap.", true)?;
    f.create(&second, "Repair session token refresh", "Same gap.", true)?;
    let report = f.report()?;
    assert!(groups(&report, "high_groups").is_empty());
    assert!(groups(&report, "low_groups").is_empty());
    assert_eq!(
        report["workspaces_covered"]
            .as_array()
            .expect("covered")
            .len(),
        2
    );
    let skipped = report["skipped"].as_array().expect("skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["workspace"], legacy.to_string_lossy().as_ref());
    assert!(skipped[0]["reason"]
        .as_str()
        .expect("reason")
        .contains("bead-rs"));
    Ok(())
}

#[test]
fn apply_closes_only_named_group_and_preserves_survivor_labels() -> Result<()> {
    let f = Fixture::new()?;
    let w = f.workspace("apply")?;
    let survivor = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let duplicate = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let other_a = f.create(
        &w,
        "Repair queue dispatch panic",
        &evidence(&["src/queue.rs"]),
        true,
    )?;
    let other_b = f.create(
        &w,
        "Repair queue dispatch panic",
        &evidence(&["src/queue.rs"]),
        true,
    )?;
    f.bead(
        &w,
        &["update", &survivor, "--notes", "Existing operator note."],
    )?;
    let report = f.report()?;
    assert_eq!(groups(&report, "high_groups").len(), 2);
    let chosen = groups(&report, "high_groups")
        .iter()
        .find(|group| ids(group).contains(duplicate.as_str()))
        .expect("session group");
    assert_eq!(chosen["survivor"], survivor);
    let output = f.apply(chosen["id"].as_str().expect("group ID"))?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcomes: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(outcomes[0]["outcome"], "applied");
    let closed = f.show(&w, &duplicate)?;
    assert_eq!(closed["status"], "closed");
    assert!(closed["dependencies"]
        .as_array()
        .expect("dependencies")
        .iter()
        .any(|edge| edge["blocker"] == survivor && edge["kind"] == "relates_to"));
    let kept = f.show(&w, &survivor)?;
    assert_eq!(kept["status"], "open");
    let notes = kept["notes"].as_str().expect("survivor notes");
    assert!(notes.starts_with("Existing operator note.\n"));
    assert!(notes.contains(&format!(
        "dedupe group {}",
        chosen["id"].as_str().expect("group ID")
    )));
    assert!(notes.contains(&duplicate));
    for label in ["proposal-pending", "human"] {
        assert!(kept["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .any(|item| item == label));
    }
    assert_eq!(f.show(&w, &other_a)?["status"], "open");
    assert_eq!(f.show(&w, &other_b)?["status"], "open");
    Ok(())
}

#[test]
fn apply_refuses_group_after_assignee_change_without_closes() -> Result<()> {
    let f = Fixture::new()?;
    let w = f.workspace("assigned")?;
    let a = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let b = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let report = f.report()?;
    let id = groups(&report, "high_groups")[0]["id"]
        .as_str()
        .expect("group ID");
    f.bead(&w, &["update", &b, "--assignee", "operator"])?;
    let output = f.apply(id)?;
    assert!(!output.status.success());
    let outcomes: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(outcomes[0]["outcome"], "refused");
    assert_eq!(f.show(&w, &a)?["status"], "open");
    assert_eq!(f.show(&w, &b)?["status"], "open");

    // A second fixture changes a member after apply has recomputed the group
    // from `list`, but before its first guarded `show`. The group must refuse
    // without closing either bead.
    let f = Fixture::new()?;
    let w = f.workspace("revision")?;
    let first = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let second = f.create(
        &w,
        "Repair session token refresh",
        &evidence(&["src/session.rs"]),
        true,
    )?;
    let report = f.report()?;
    let group_id = groups(&report, "high_groups")[0]["id"]
        .as_str()
        .expect("group ID");
    let wrapper = f.home.join("race-bead");
    let marker = f.home.join("race-once");
    fs::write(&marker, "armed")?;
    fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = show ] && [ -e '{}' ]; then\n  rm '{}'\n  '{}' update '{}' --append-notes 'Concurrent note' >/dev/null || exit 1\nfi\nexec '{}' \"$@\"\n",
        marker.display(), marker.display(), f.bead.display(), second, f.bead.display()
    ))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&wrapper)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions)?;
    }
    fs::write(w.join(".needle.yaml"), format!(
        "bead_cli:\n  backend: bead-rs\n  path: {}\nstrands:\n  weave:\n    duplicate_similarity_threshold: 0.5\n",
        wrapper.display()
    ))?;
    let output = f.apply(group_id)?;
    assert!(!output.status.success());
    let outcomes: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(outcomes[0]["outcome"], "refused");
    assert!(outcomes[0]["detail"]
        .as_str()
        .expect("detail")
        .contains("revision"));
    assert_eq!(f.show(&w, &first)?["status"], "open");
    assert_eq!(f.show(&w, &second)?["status"], "open");
    Ok(())
}
