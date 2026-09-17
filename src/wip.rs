//! Capture uncommitted work left by an attempt that cannot finish normally.
//!
//! The shared checkout is deliberately treated as hostile here: a timeout
//! must not sweep another worker's dirty paths into its recovery stash. The
//! candidate set is the current Git diff/untracked set, narrowed to paths
//! named by Edit/Write tool calls or paths whose mtime falls in this attempt's
//! wall-clock window. Paths that were already dirty at dispatch are only
//! accepted when the attempt explicitly named them.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable reference to a captured working-tree patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WipPatch {
    /// Path relative to the attempt's workspace.
    pub path: String,
    /// Size of the durable patch file in bytes.
    pub bytes: u64,
}

/// Inputs needed to capture one attempt's working-tree edits.
pub struct Capture<'a> {
    pub workspace: &'a Path,
    pub bead_id: &'a str,
    pub attempt_id: &'a str,
    pub started_at: Option<DateTime<Utc>>,
    pub preexisting_paths: &'a [String],
    pub transcript: &'a str,
}

/// Capture the attempt's own uncommitted paths and preserve them in both the
/// trace directory and a named Git stash.
pub async fn capture(input: Capture<'_>) -> Result<Option<WipPatch>> {
    let ended_at = SystemTime::now();
    let named_paths = transcript_edit_write_paths(input.workspace, input.transcript);
    let candidates = current_paths(input.workspace).await?;
    let preexisting: BTreeSet<&str> = input.preexisting_paths.iter().map(String::as_str).collect();
    let start = input.started_at.map(system_time);

    let mut selected = Vec::new();
    for path in candidates {
        if is_internal_path(&path) {
            continue;
        }
        let explicitly_named = named_paths.contains(&path);
        let modified_in_window = start.is_some_and(|started| {
            modified_time(input.workspace.join(&path))
                .is_some_and(|mtime| mtime >= started && mtime <= ended_at)
        });

        // A path dirty before dispatch is not ours unless the transcript
        // identifies it. This is the important shared-checkout guard: mtime
        // alone must not turn another worker's old edit into our stash.
        if explicitly_named || (!preexisting.contains(path.as_str()) && modified_in_window) {
            selected.push(path);
        }
    }
    selected.sort();
    selected.dedup();

    if selected.is_empty() {
        return Ok(None);
    }

    let selected_os: Vec<OsString> = selected.iter().map(OsString::from).collect();
    let tracked_diff = git_diff(input.workspace, &selected_os).await?;
    let untracked = untracked_paths(input.workspace, &selected).await?;
    let patch_rel = PathBuf::from(".beads")
        .join("traces")
        .join(input.bead_id)
        .join(format!("wip-{}.patch", input.attempt_id));
    let patch_path = input.workspace.join(&patch_rel);
    if let Some(parent) = patch_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating WIP trace directory {}", parent.display()))?;
    }

    // The comments make the untracked recovery paths durable evidence while
    // leaving the tracked portion as a normal `git diff` that `git apply
    // --3way` can consume. The stash holds the untracked contents.
    let mut patch = String::new();
    patch.push_str("# NEEDLE WIP patch; review before applying.\n");
    for path in &untracked {
        patch.push_str("# untracked: ");
        patch.push_str(path);
        patch.push('\n');
    }
    if !tracked_diff.is_empty() {
        patch.push('\n');
        patch.push_str(&tracked_diff);
    }
    tokio::fs::write(&patch_path, patch.as_bytes())
        .await
        .with_context(|| format!("writing WIP patch {}", patch_path.display()))?;

    let stash_name = format!("needle:{}:{}", input.bead_id, input.attempt_id);
    let stash_output = git_stash(input.workspace, &stash_name, &selected_os).await?;
    if !stash_output.status.success() {
        let stderr = String::from_utf8_lossy(&stash_output.stderr);
        bail!("git stash failed for {}: {}", stash_name, stderr.trim());
    }

    let bytes = tokio::fs::metadata(&patch_path)
        .await
        .with_context(|| format!("statting WIP patch {}", patch_path.display()))?
        .len();
    Ok(Some(WipPatch {
        path: patch_rel.to_string_lossy().into_owned(),
        bytes,
    }))
}

fn system_time(timestamp: DateTime<Utc>) -> SystemTime {
    timestamp
        .timestamp_nanos_opt()
        .map(|nanos| {
            if nanos >= 0 {
                SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos as u64)
            } else {
                SystemTime::UNIX_EPOCH - Duration::from_nanos(nanos.unsigned_abs())
            }
        })
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn modified_time(path: PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn is_internal_path(path: &str) -> bool {
    path == ".beads" || path.starts_with(".beads/") || path == ".needle-predispatch-sha"
}

async fn current_paths(workspace: &Path) -> Result<Vec<String>> {
    let tracked = git_output(workspace, ["diff", "--name-only", "-z", "HEAD"]).await?;
    if !tracked.status.success() {
        bail!(
            "git diff --name-only failed: {}",
            String::from_utf8_lossy(&tracked.stderr).trim()
        );
    }
    let untracked = git_output(
        workspace,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;
    if !untracked.status.success() {
        bail!(
            "git ls-files --others failed: {}",
            String::from_utf8_lossy(&untracked.stderr).trim()
        );
    }

    let mut paths = BTreeSet::new();
    for raw in tracked
        .stdout
        .split(|byte| *byte == 0)
        .chain(untracked.stdout.split(|byte| *byte == 0))
    {
        if raw.is_empty() {
            continue;
        }
        let path = String::from_utf8(raw.to_vec()).context("Git returned a non-UTF-8 path")?;
        if !is_internal_path(&path) {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

async fn untracked_paths(workspace: &Path, selected: &[String]) -> Result<Vec<String>> {
    let output = git_output(
        workspace,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;
    if !output.status.success() {
        bail!(
            "git ls-files --others failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let selected: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    let mut paths = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = String::from_utf8(raw.to_vec()).context("Git returned a non-UTF-8 path")?;
        if selected.contains(path.as_str()) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

async fn git_diff(workspace: &Path, selected: &[OsString]) -> Result<String> {
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--binary"),
        OsString::from("HEAD"),
        OsString::from("--"),
    ];
    args.extend(selected.iter().cloned());
    let output = git_output_os(workspace, args).await?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("Git returned a non-UTF-8 diff")
}

async fn git_stash(
    workspace: &Path,
    name: &str,
    selected: &[OsString],
) -> Result<std::process::Output> {
    let mut args = vec![
        OsString::from("stash"),
        OsString::from("push"),
        OsString::from("--include-untracked"),
        OsString::from("--message"),
        OsString::from(name),
        OsString::from("--"),
    ];
    args.extend(selected.iter().cloned());
    git_output_os(workspace, args).await
}

async fn git_output<const N: usize>(
    workspace: &Path,
    args: [&str; N],
) -> Result<std::process::Output> {
    git_output_os(workspace, args.into_iter().map(OsString::from).collect()).await
}

async fn git_output_os(workspace: &Path, args: Vec<OsString>) -> Result<std::process::Output> {
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Git WIP capture timed out")?
    .context("running Git WIP capture command")
}

fn transcript_edit_write_paths(workspace: &Path, transcript: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for line in transcript.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        collect_tool_paths(&value, workspace, &mut paths);
    }
    paths
}

fn collect_tool_paths(value: &Value, workspace: &Path, paths: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            let tool = map
                .get("tool")
                .or_else(|| map.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(tool.to_ascii_lowercase().as_str(), "edit" | "write") {
                let path = map.get("path").and_then(Value::as_str).or_else(|| {
                    map.get("input")
                        .and_then(|input| input.get("file_path").or_else(|| input.get("path")))
                        .and_then(Value::as_str)
                });
                if let Some(path) = path.and_then(|path| normalize_path(workspace, path)) {
                    if !is_internal_path(&path) {
                        paths.insert(path);
                    }
                }
            }
            for child in map.values() {
                collect_tool_paths(child, workspace, paths);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_tool_paths(child, workspace, paths);
            }
        }
        _ => {}
    }
}

fn normalize_path(workspace: &Path, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace).ok()?
    } else {
        path
    };
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        return None;
    }
    Some(normalized.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{set_file_mtime, FileTime};
    use std::process::Command;

    fn git(workspace: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn captures_two_attempt_files_and_excludes_old_unrelated_dirty_file() {
        let workspace = tempfile::tempdir().expect("workspace");
        git(workspace.path(), &["init", "-q"]);
        git(
            workspace.path(),
            &["config", "user.email", "test@example.com"],
        );
        git(workspace.path(), &["config", "user.name", "Test"]);
        std::fs::write(workspace.path().join("one.txt"), "one\n").unwrap();
        std::fs::write(workspace.path().join("two.txt"), "two\n").unwrap();
        std::fs::write(workspace.path().join("unrelated.txt"), "old\n").unwrap();
        git(workspace.path(), &["add", "."]);
        git(workspace.path(), &["commit", "-qm", "base"]);

        std::fs::write(workspace.path().join("unrelated.txt"), "other worker\n").unwrap();
        set_file_mtime(
            workspace.path().join("unrelated.txt"),
            FileTime::from_unix_time(1, 0),
        )
        .unwrap();
        std::fs::write(workspace.path().join("one.txt"), "one changed\n").unwrap();
        std::fs::write(workspace.path().join("two.txt"), "two changed\n").unwrap();

        let patch = capture(Capture {
            workspace: workspace.path(),
            bead_id: "needle-fixture",
            attempt_id: "attempt-1",
            started_at: Some(Utc::now() - chrono::Duration::minutes(1)),
            preexisting_paths: &[],
            transcript: concat!(
                r#"{"type":"tool_call","tool":"Edit","path":"one.txt"}"#,
                "\n",
                r#"{"type":"tool_call","tool":"Write","path":"two.txt"}"#,
            ),
        })
        .await
        .expect("capture succeeds")
        .expect("patch exists");
        assert!(patch.path.ends_with("wip-attempt-1.patch"));
        let patch_text = std::fs::read_to_string(workspace.path().join(&patch.path)).unwrap();
        assert!(patch_text.contains("one changed"));
        assert!(patch_text.contains("two changed"));
        assert!(!patch_text.contains("other worker"));

        let stash = Command::new("git")
            .args(["stash", "list"])
            .current_dir(workspace.path())
            .output()
            .unwrap();
        let stash = String::from_utf8_lossy(&stash.stdout);
        assert!(stash.contains("needle:needle-fixture:attempt-1"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("one.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("unrelated.txt")).unwrap(),
            "other worker\n"
        );

        git(workspace.path(), &["apply", "--3way", &patch.path]);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("one.txt")).unwrap(),
            "one changed\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("two.txt")).unwrap(),
            "two changed\n"
        );
    }
}
