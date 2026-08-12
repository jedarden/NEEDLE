//! Public-CLI proof for the bead-forge -> bead-rs rehydration playbook.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(binary: &Path, workspace: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(binary)
        .args(args)
        .current_dir(workspace)
        .output()
        .with_context(|| format!("failed to execute {} {args:?}", binary.display()))?;
    if !output.status.success() {
        bail!(
            "{} {args:?} failed: {}",
            binary.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("CLI stdout was not UTF-8")
}

fn json_lines(output: &str) -> Result<Vec<Value>> {
    let mut records = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).context("invalid JSON line")?;
        if let Value::Array(values) = value {
            records.extend(values);
        } else {
            records.push(value);
        }
    }
    Ok(records)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn source_edges(records: &[Value]) -> BTreeSet<(String, String, String)> {
    records
        .iter()
        .flat_map(|record| {
            let blocked = record["id"].as_str().unwrap_or_default().to_string();
            record["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |dependency| {
                    Some((
                        blocked.clone(),
                        dependency["id"].as_str()?.to_string(),
                        dependency["dependency_type"]
                            .as_str()
                            .unwrap_or("blocks")
                            .to_string(),
                    ))
                })
        })
        .collect()
}

fn destination_edges(records: &[Value]) -> BTreeSet<(String, String, String)> {
    records
        .iter()
        .flat_map(|record| {
            let blocked = record["id"].as_str().unwrap_or_default().to_string();
            record["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |dependency| {
                    Some((
                        blocked.clone(),
                        dependency["blocker"].as_str()?.to_string(),
                        dependency["kind"].as_str().unwrap_or("blocks").to_string(),
                    ))
                })
        })
        .collect()
}

fn verify_reconciliation(
    source: &[Value],
    destination: &[Value],
    mapping: &BTreeMap<String, String>,
) -> Result<()> {
    if source.len() != destination.len() {
        bail!(
            "count mismatch: source={}, destination={}",
            source.len(),
            destination.len()
        );
    }
    for record in source {
        let id = record["id"].as_str().context("source ID missing")?;
        if !mapping.contains_key(id) {
            bail!("source issue {id} has no reconciliation disposition");
        }
    }
    let expected = source_edges(source)
        .into_iter()
        .map(|(blocked, blocker, kind)| {
            Ok((
                mapping
                    .get(&blocked)
                    .cloned()
                    .with_context(|| format!("unmapped blocked issue {blocked}"))?,
                mapping
                    .get(&blocker)
                    .cloned()
                    .with_context(|| format!("unmapped blocker {blocker}"))?,
                kind,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let actual = destination_edges(destination);
    if expected != actual {
        bail!("dependency graph mismatch: expected {expected:?}, actual {actual:?}");
    }
    Ok(())
}

fn create_native(bead: &Path, workspace: &Path, source: &Value) -> Result<String> {
    let title = source["title"].as_str().context("title missing")?;
    let description = source["description"].as_str().unwrap_or_default();
    let priority = source["priority"].as_u64().unwrap_or(2).min(4).to_string();
    let issue_type = source["issue_type"].as_str().unwrap_or("task");
    let mut args = vec![
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--description".to_string(),
        description.to_string(),
        "--priority".to_string(),
        priority,
        "--issue-type".to_string(),
        issue_type.to_string(),
    ];
    for label in source["labels"].as_array().into_iter().flatten() {
        if let Some(label) = label.as_str() {
            args.extend(["--label".to_string(), label.to_string()]);
        }
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(run(bead, workspace, &refs)?.trim().to_string())
}

fn apply_lifecycle(bead: &Path, workspace: &Path, source: &Value, id: &str) -> Result<()> {
    let status = source["status"].as_str().unwrap_or("open");
    match status {
        "open" => {}
        "in_progress" => {
            let actor = source["assignee"].as_str().unwrap_or("rehydrated-owner");
            run(
                bead,
                workspace,
                &["update", id, "--status", "in_progress", "--assignee", actor],
            )?;
        }
        "deferred" => {
            run(bead, workspace, &["update", id, "--status", "deferred"])?;
        }
        "closed" | "completed" => {
            let reason = source["close_reason"]
                .as_str()
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or("Rehydrated from closed source issue");
            run(bead, workspace, &["close", id, "--reason", reason])?;
        }
        "blocked" => {
            // A dependency-blocked bf issue becomes an open base issue plus
            // its blocker edge. Only edge-free blocked issues are manual.
            let has_blocker = source["dependencies"]
                .as_array()
                .is_some_and(|dependencies| !dependencies.is_empty());
            if !has_blocker {
                run(bead, workspace, &["update", id, "--status", "blocked"])?;
            }
        }
        other => bail!("source status {other:?} requires an unresolved disposition"),
    }
    Ok(())
}

fn rehydrate(
    bead: &Path,
    destination: &Path,
    source: &[Value],
) -> Result<BTreeMap<String, String>> {
    run(bead, destination, &["init", "--prefix", "native"])?;
    let mut mapping = BTreeMap::new();
    for record in source {
        mapping.insert(
            record["id"]
                .as_str()
                .context("source ID missing")?
                .to_string(),
            create_native(bead, destination, record)?,
        );
    }
    for (blocked, blocker, kind) in source_edges(source) {
        run(
            bead,
            destination,
            &[
                "dep",
                "add",
                &mapping[&blocked],
                &mapping[&blocker],
                "--kind",
                &kind,
            ],
        )?;
    }
    for record in source {
        let source_id = record["id"].as_str().context("source ID missing")?;
        apply_lifecycle(bead, destination, record, &mapping[source_id])?;
    }
    Ok(mapping)
}

#[test]
fn verifier_rejects_omission_and_edge_inversion() {
    let source = vec![
        serde_json::json!({"id":"bf-a","dependencies":[]}),
        serde_json::json!({"id":"bf-b","dependencies":[{"id":"bf-a","dependency_type":"blocks"}]}),
    ];
    let destination = vec![
        serde_json::json!({"id":"bead-a","dependencies":[]}),
        serde_json::json!({"id":"bead-b","dependencies":[{"blocker":"bead-a","kind":"blocks"}]}),
    ];
    let mapping = BTreeMap::from([
        ("bf-a".to_string(), "bead-a".to_string()),
        ("bf-b".to_string(), "bead-b".to_string()),
    ]);
    verify_reconciliation(&source, &destination, &mapping).unwrap();

    let omitted = BTreeMap::from([("bf-a".to_string(), "bead-a".to_string())]);
    assert!(verify_reconciliation(&source, &destination, &omitted).is_err());

    let inverted = vec![
        serde_json::json!({"id":"bead-a","dependencies":[{"blocker":"bead-b","kind":"blocks"}]}),
        serde_json::json!({"id":"bead-b","dependencies":[]}),
    ];
    assert!(verify_reconciliation(&source, &inverted, &mapping).is_err());
}

#[test]
#[ignore = "release gate requiring BF_BIN and BEAD_RS_BIN"]
fn public_cli_rehydration_checkpoint_restore_and_rollback() {
    let bf = PathBuf::from(std::env::var_os("BF_BIN").expect("BF_BIN is required"));
    let bead = PathBuf::from(std::env::var_os("BEAD_RS_BIN").expect("BEAD_RS_BIN is required"));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    let restore = root.path().join("restore");
    let source_backup = root.path().join("source-backup");
    for path in [&source, &destination, &restore, &source_backup] {
        fs::create_dir_all(path).unwrap();
    }

    run(&bf, &source, &["init", "--prefix", "legacy"]).unwrap();
    let blocker = run(
        &bf,
        &source,
        &[
            "create",
            "--title",
            "Blocker",
            "--description",
            "first",
            "--priority",
            "1",
            "--label",
            "migration-fixture",
        ],
    )
    .unwrap()
    .trim()
    .to_string();
    run(
        &bf,
        &source,
        &["claim", "--assignee", "fixture-worker", "--json"],
    )
    .unwrap();
    let blocked = run(
        &bf,
        &source,
        &[
            "create",
            "--title",
            "Dependent",
            "--description",
            "second",
            "--priority",
            "2",
        ],
    )
    .unwrap()
    .trim()
    .to_string();
    let closed = run(
        &bf,
        &source,
        &["create", "--title", "Closed", "--priority", "3"],
    )
    .unwrap()
    .trim()
    .to_string();
    let deferred = run(
        &bf,
        &source,
        &["create", "--title", "Deferred", "--priority", "4"],
    )
    .unwrap()
    .trim()
    .to_string();
    run(
        &bf,
        &source,
        &["dep", "add", &blocker, "--blocks", &blocked],
    )
    .unwrap();
    run(
        &bf,
        &source,
        &["close", &closed, "--reason", "fixture complete"],
    )
    .unwrap();
    run(&bf, &source, &["update", &deferred, "--status", "deferred"]).unwrap();
    run(&bf, &source, &["sync", "--flush-only"]).unwrap();
    copy_tree(&source.join(".beads"), &source_backup.join(".beads")).unwrap();

    let before = run(
        &bf,
        &source,
        &["list", "--all", "--limit", "999999", "--json"],
    )
    .unwrap();
    let source_records = json_lines(&before).unwrap();
    let mapping = rehydrate(&bead, &destination, &source_records).unwrap();
    let evidence = root.path().join("evidence");
    fs::create_dir_all(&evidence).unwrap();
    fs::write(evidence.join("source.jsonl"), &before).unwrap();
    fs::write(
        evidence.join("reconciliation.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "source_repository": "fixture",
            "source_commit": "fixture",
            "issues": mapping,
        }))
        .unwrap(),
    )
    .unwrap();
    let gitleaks = which::which("gitleaks").expect("gitleaks is required for the release gate");
    run(
        &gitleaks,
        root.path(),
        &[
            "detect",
            "--no-git",
            "--source",
            evidence.to_str().unwrap(),
            "--redact",
            "--no-banner",
        ],
    )
    .unwrap();
    let destination_records = json_lines(
        &run(
            &bead,
            &destination,
            &["list", "--json", "--limit", "999999"],
        )
        .unwrap(),
    )
    .unwrap();
    verify_reconciliation(&source_records, &destination_records, &mapping).unwrap();
    run(&bead, &destination, &["doctor"]).unwrap();

    let source_ready =
        json_lines(&run(&bf, &source, &["ready", "--limit", "999999", "--json"]).unwrap())
            .unwrap()
            .into_iter()
            .map(|record| mapping[record["id"].as_str().unwrap()].clone())
            .collect::<BTreeSet<_>>();
    let destination_ready = json_lines(
        &run(
            &bead,
            &destination,
            &["list", "--ready", "--json", "--limit", "999999"],
        )
        .unwrap(),
    )
    .unwrap()
    .into_iter()
    .filter_map(|record| record["id"].as_str().map(ToOwned::to_owned))
    .collect::<BTreeSet<_>>();
    assert_eq!(source_ready, destination_ready);

    run(&bead, &destination, &["sync", "flush-only"]).unwrap();
    fs::create_dir_all(restore.join(".beads")).unwrap();
    fs::copy(
        destination.join(".beads/config.json"),
        restore.join(".beads/config.json"),
    )
    .unwrap();
    copy_tree(
        &destination.join(".beads/checkpoint"),
        &restore.join(".beads/checkpoint"),
    )
    .unwrap();
    run(&bead, &restore, &["init"]).unwrap();
    run(
        &bead,
        &restore,
        &[
            "sync",
            "import-only",
            "--input",
            ".beads/checkpoint",
            "--restore-into-empty",
            "--actor",
            "rehydration-test",
        ],
    )
    .unwrap();
    run(&bead, &restore, &["doctor"]).unwrap();
    let restored =
        json_lines(&run(&bead, &restore, &["list", "--json", "--limit", "999999"]).unwrap())
            .unwrap();
    verify_reconciliation(&source_records, &restored, &mapping).unwrap();

    // Rollback proof: destination work never mutates the source, and its
    // preserved public-CLI snapshot remains byte-for-byte reproducible.
    let after = run(
        &bf,
        &source,
        &["list", "--all", "--limit", "999999", "--json"],
    )
    .unwrap();
    assert_eq!(before, after);
    let backup = run(
        &bf,
        &source_backup,
        &["list", "--all", "--limit", "999999", "--json"],
    )
    .unwrap();
    assert_eq!(before, backup);
}
