//! Audited, marker-managed promotion of reviewed candidate lessons.
//!
//! Candidate lessons are untrusted retrieval records until an operator reviews
//! them. This module is the deliberately separate write boundary: it validates
//! a frontmatter document, projects only its markdown body into a named
//! instruction file, and keeps enough receipt data to restore the exact bytes
//! that were present before promotion.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::policy::RESOLVED_POLICY_VERSION;

const RECEIPT_SCHEMA_VERSION: u8 = 1;
const PROMOTION_DIRECTORY: &str = ".needle/promotions";
const START_MARKER_PREFIX: &str = "<!-- needle-lesson:";
const END_MARKER_PREFIX: &str = "<!-- /needle-lesson";

/// One expired marker found in an instruction source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpiredMarker {
    pub(crate) id: String,
    pub(crate) expired_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ValidatedLesson {
    id: String,
    fingerprint: String,
    scope: String,
    expiry: Option<DateTime<Utc>>,
    reviewed_by: String,
    body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromotionReceipt {
    schema_version: u8,
    lesson_id: String,
    target: String,
    policy_version: String,
    promoted_at: DateTime<Utc>,
    target_existed: bool,
    /// The exact fenced block that was replaced, if this was a replacement.
    previous_block: Option<String>,
    /// The exact target bytes before promotion. This is what makes demotion
    /// byte-identical even when promotion appended a separator or created a
    /// previously absent target file.
    previous_content: String,
    /// The bytes written by promotion; demote refuses to clobber later edits.
    promoted_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expiry {
    Never,
    At(DateTime<Utc>),
}

impl Expiry {
    fn date(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Never => None,
            Self::At(value) => Some(value),
        }
    }
}

/// Promote a reviewed lesson into a repository instruction file.
pub(crate) fn promote(lesson_file: &Path, repo: &Path, target: Option<&Path>) -> Result<()> {
    let repo = repository_root(repo)?;
    let lesson_file = lesson_file
        .canonicalize()
        .with_context(|| format!("resolve lesson file {}", lesson_file.display()))?;
    let raw = fs::read_to_string(&lesson_file)
        .with_context(|| format!("read lesson file {}", lesson_file.display()))?;
    let lesson = validate_lesson(&raw, &lesson_file, &repo)?;
    let target = target_path(&repo, target)?;
    let previous_content = read_target(&target)?;
    let previous_block = locate_block(&previous_content, &lesson.id)?.map(|(_, _, block)| block);
    let promoted_content = project_block(&previous_content, &lesson)?;

    let receipt = PromotionReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        lesson_id: lesson.id.clone(),
        target: relative_target(&repo, &target)?,
        policy_version: RESOLVED_POLICY_VERSION.to_owned(),
        promoted_at: Utc::now(),
        target_existed: target.exists(),
        previous_block,
        previous_content,
        promoted_content: promoted_content.clone(),
    };

    write_target(&target, &promoted_content)?;
    let receipt_path = receipt_path(&repo, &lesson.id)?;
    write_json_atomic(&receipt_path, &receipt)
        .with_context(|| format!("write promotion receipt {}", receipt_path.display()))?;

    println!(
        "promoted lesson {} into {} (policy {})",
        lesson.id,
        target.display(),
        RESOLVED_POLICY_VERSION
    );
    Ok(())
}

/// Restore the exact target bytes recorded in the latest receipt for an ID.
pub(crate) fn demote(id: &str, repo: &Path) -> Result<()> {
    validate_id(id)?;
    let repo = repository_root(repo)?;
    let path = receipt_path(&repo, id)?;
    ensure!(
        path.is_file(),
        "no promotion receipt found for lesson {id:?}"
    );
    let receipt: PromotionReceipt = serde_json::from_str(
        &fs::read_to_string(&path)
            .with_context(|| format!("read promotion receipt {}", path.display()))?,
    )
    .with_context(|| format!("parse promotion receipt {}", path.display()))?;
    ensure!(
        receipt.schema_version == RECEIPT_SCHEMA_VERSION,
        "unsupported promotion receipt schema {}",
        receipt.schema_version
    );
    ensure!(
        receipt.lesson_id == id,
        "promotion receipt lesson ID does not match {id:?}"
    );

    let target = target_path(&repo, Some(Path::new(&receipt.target)))?;
    let current = read_target(&target)?;
    ensure!(
        current == receipt.promoted_content,
        "refusing to demote lesson {id:?}: target {} changed after promotion",
        target.display()
    );

    if receipt.target_existed {
        write_target(&target, &receipt.previous_content)?;
    } else if target.exists() {
        fs::remove_file(&target)
            .with_context(|| format!("remove promoted target {}", target.display()))?;
    }

    println!("demoted lesson {} from {}", id, target.display());
    Ok(())
}

/// Find expired lesson markers in one policy source.
pub(crate) fn expired_markers(content: &str, now: DateTime<Utc>) -> Vec<ExpiredMarker> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = content[cursor..].find(START_MARKER_PREFIX) {
        let start = cursor + relative_start;
        let Some((end, block)) = block_at(content, start) else {
            break;
        };
        let Some(id) = marker_id(block) else {
            cursor = end;
            continue;
        };
        if let Some(expiry) = marker_expiry(block) {
            if expiry <= now {
                result.push(ExpiredMarker {
                    id,
                    expired_at: expiry,
                });
            }
        }
        cursor = end;
    }
    result
}

fn validate_lesson(raw: &str, lesson_file: &Path, repo: &Path) -> Result<ValidatedLesson> {
    let (frontmatter, body) = split_frontmatter(raw).with_context(|| {
        format!(
            "parse CandidateLesson frontmatter in {}",
            lesson_file.display()
        )
    })?;
    let document: Value = serde_yaml::from_str(frontmatter)
        .context("CandidateLesson frontmatter is not valid YAML")?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("CandidateLesson frontmatter must be a YAML mapping"))?;

    if let Some(schema) = scalar(mapping, &["schema"]) {
        ensure!(
            schema == "needle.candidate-lesson/v1",
            "unsupported CandidateLesson schema {schema:?}"
        );
    }

    let id = required_scalar(mapping, &["id", "candidate_id"], "id")?;
    validate_id(&id)?;
    let fingerprint = required_scalar(mapping, &["fingerprint"], "fingerprint")?;
    validate_single_line(&fingerprint, "fingerprint")?;

    let evidence_refs = evidence_refs(mapping)?;
    ensure!(
        !evidence_refs.is_empty(),
        "CandidateLesson evidence refs must not be empty"
    );
    for reference in &evidence_refs {
        ensure!(
            evidence_resolves(reference, lesson_file, repo),
            "CandidateLesson evidence ref {reference:?} does not resolve from {}",
            repo.display()
        );
    }

    let scope_value = mapping_value(mapping, &["scope"])
        .ok_or_else(|| anyhow::anyhow!("CandidateLesson frontmatter is missing scope"))?;
    let scope = scope_text(scope_value)?;

    let expiry_value = mapping_value(mapping, &["expiry"])
        .ok_or_else(|| anyhow::anyhow!("CandidateLesson frontmatter is missing expiry"))?;
    let expiry = parse_expiry(expiry_value, "expiry")?;

    let reviewed_by = required_scalar(mapping, &["reviewed_by"], "reviewed_by")?;
    validate_single_line(&reviewed_by, "reviewed_by")?;
    ensure!(
        !body.trim().is_empty(),
        "CandidateLesson body must not be empty"
    );
    ensure!(
        !body.contains(START_MARKER_PREFIX) && !body.contains(END_MARKER_PREFIX),
        "CandidateLesson body must not contain needle lesson fence markers"
    );

    Ok(ValidatedLesson {
        id,
        fingerprint,
        scope,
        expiry: expiry.date(),
        reviewed_by,
        body: body.trim().to_owned(),
    })
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str)> {
    let opening = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow::anyhow!("CandidateLesson must start with YAML frontmatter"))?;
    let closing = opening
        .find("\n---\n")
        .map(|offset| (offset, 5))
        .or_else(|| opening.find("\n---\r\n").map(|offset| (offset, 6)))
        .ok_or_else(|| anyhow::anyhow!("CandidateLesson frontmatter is not closed by ---"))?;
    let (frontmatter, rest) = opening.split_at(closing.0);
    Ok((frontmatter, &rest[closing.1..]))
}

fn mapping_value<'a>(mapping: &'a Mapping, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| mapping.get(Value::String((*key).to_owned())))
}

fn scalar(mapping: &Mapping, keys: &[&str]) -> Option<String> {
    mapping_value(mapping, keys).and_then(|value| value.as_str().map(str::trim).map(str::to_owned))
}

fn required_scalar(mapping: &Mapping, keys: &[&str], label: &str) -> Result<String> {
    let value = scalar(mapping, keys).ok_or_else(|| {
        anyhow::anyhow!("CandidateLesson frontmatter is missing non-empty {label}")
    })?;
    ensure!(
        !value.is_empty(),
        "CandidateLesson {label} must not be empty"
    );
    Ok(value)
}

fn validate_single_line(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.contains(['\n', '\r']) && !value.contains("-->"),
        "CandidateLesson {label} must be a single line"
    );
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(!id.is_empty(), "lesson ID must not be empty");
    ensure!(
        id.chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character)),
        "lesson ID {id:?} contains unsupported filename or marker characters"
    );
    Ok(())
}

fn evidence_refs(mapping: &Mapping) -> Result<Vec<String>> {
    let mut refs = Vec::new();
    for key in ["evidence_refs", "evidence"] {
        if let Some(value) = mapping_value(mapping, &[key]) {
            collect_strings(value, &mut refs);
        }
    }
    refs.sort();
    refs.dedup();
    for reference in &refs {
        ensure!(
            !reference.trim().is_empty(),
            "CandidateLesson evidence ref is empty"
        );
        validate_single_line(reference, "evidence ref")?;
    }
    Ok(refs)
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.trim().to_owned()),
        Value::Sequence(values) => values
            .iter()
            .for_each(|value| collect_strings(value, output)),
        Value::Mapping(values) => values
            .values()
            .for_each(|value| collect_strings(value, output)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Tagged(_) => {}
    }
}

fn scope_text(value: &Value) -> Result<String> {
    let text = match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Sequence(_) | Value::Mapping(_) => serde_json::to_string(value)
            .context("serialize CandidateLesson scope")?
            .trim()
            .to_owned(),
        Value::Null | Value::Bool(_) | Value::Number(_) => String::new(),
        Value::Tagged(_) => serde_yaml::to_string(value)
            .context("serialize CandidateLesson scope")?
            .trim()
            .to_owned(),
    };
    ensure!(
        !text.is_empty() && text != "{}" && text != "[]" && !text.contains("-->"),
        "CandidateLesson scope must not be empty"
    );
    Ok(text)
}

fn parse_expiry(value: &Value, label: &str) -> Result<Expiry> {
    let Some(value) = value.as_str() else {
        ensure!(
            value.is_null(),
            "CandidateLesson {label} must be an RFC3339 date, YYYY-MM-DD, or null"
        );
        return Ok(Expiry::Never);
    };
    if value.trim().is_empty()
        || value.eq_ignore_ascii_case("never")
        || value.eq_ignore_ascii_case("none")
    {
        return Ok(Expiry::Never);
    }
    if let Ok(expiry) = DateTime::parse_from_rfc3339(value) {
        return Ok(Expiry::At(expiry.with_timezone(&Utc)));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").with_context(|| {
        format!("CandidateLesson {label} must be an RFC3339 date, YYYY-MM-DD, or null")
    })?;
    Ok(Expiry::At(DateTime::<Utc>::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid CandidateLesson expiry date"))?,
        Utc,
    )))
}

fn evidence_resolves(reference: &str, lesson_file: &Path, repo: &Path) -> bool {
    let reference = reference.strip_prefix("file:").unwrap_or(reference);
    let candidate_paths = [
        lesson_file.parent().unwrap_or(repo).join(reference),
        repo.join(reference),
    ];
    if candidate_paths.iter().any(|path| path.exists()) {
        return true;
    }
    if reference.starts_with("att-") {
        return find_attempt_id(&repo.join(".beads").join("traces"), reference);
    }
    false
}

fn find_attempt_id(root: &Path, wanted: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && find_attempt_id(&path, wanted) {
            return true;
        }
        if path.file_name().and_then(|name| name.to_str()) != Some("attempts.jsonl") {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if text.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|value| {
                    value
                        .get("attempt_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some(wanted)
        }) {
            return true;
        }
    }
    false
}

fn project_block(content: &str, lesson: &ValidatedLesson) -> Result<String> {
    let block = format!(
        "{START_MARKER_PREFIX}{} -->\n<!-- needle-lesson-fingerprint:{} -->\n<!-- needle-lesson-expiry:{} -->\n<!-- needle-lesson-policy-version:{} -->\n<!-- needle-lesson-reviewed-by:{} -->\n<!-- needle-lesson-scope:{} -->\n{}\n<!-- /needle-lesson:{} -->",
        lesson.id,
        lesson.fingerprint,
        lesson
            .expiry
            .map(|expiry| expiry.to_rfc3339())
            .unwrap_or_else(|| "never".to_owned()),
        RESOLVED_POLICY_VERSION,
        lesson.reviewed_by,
        lesson.scope,
        lesson.body,
        lesson.id,
    );
    if let Some((start, end, _)) = locate_block(content, &lesson.id)? {
        let mut result = String::with_capacity(content.len() + block.len());
        result.push_str(&content[..start]);
        result.push_str(&block);
        result.push_str(&content[end..]);
        return Ok(result);
    }
    if content.is_empty() {
        Ok(block)
    } else if content.ends_with('\n') {
        Ok(format!("{content}\n{block}"))
    } else {
        Ok(format!("{content}\n\n{block}"))
    }
}

fn locate_block(content: &str, id: &str) -> Result<Option<(usize, usize, String)>> {
    let marker = format!("{START_MARKER_PREFIX}{id} -->");
    let starts = content.match_indices(&marker).collect::<Vec<_>>();
    ensure!(
        starts.len() <= 1,
        "target contains duplicate lesson marker for {id:?}"
    );
    let Some((start, _)) = starts.first().copied() else {
        return Ok(None);
    };
    let after_start = start + marker.len();
    let end_marker = format!("<!-- /needle-lesson:{id} -->");
    let end = content[after_start..]
        .find(&end_marker)
        .map(|offset| after_start + offset + end_marker.len())
        .or_else(|| {
            content[after_start..]
                .find("<!-- /needle-lesson -->")
                .map(|offset| after_start + offset + "<!-- /needle-lesson -->".len())
        })
        .ok_or_else(|| anyhow::anyhow!("lesson marker {id:?} has no closing fence"))?;
    Ok(Some((start, end, content[start..end].to_owned())))
}

fn block_at(content: &str, start: usize) -> Option<(usize, &str)> {
    let after_start = content[start..].find("-->")? + start + 3;
    let end = content[after_start..]
        .find("<!-- /needle-lesson:")
        .or_else(|| content[after_start..].find("<!-- /needle-lesson -->"))?;
    let end = after_start + end;
    let close_end = content[end..].find("-->")? + end + 3;
    Some((close_end, &content[start..close_end]))
}

fn marker_id(block: &str) -> Option<String> {
    let first = block.lines().next()?.trim();
    let id = first
        .strip_prefix(START_MARKER_PREFIX)?
        .strip_suffix("-->")?
        .trim();
    (!id.is_empty()).then(|| id.to_owned())
}

fn marker_expiry(block: &str) -> Option<DateTime<Utc>> {
    for line in block.lines() {
        let Some(value) = line
            .trim()
            .strip_prefix("<!-- needle-lesson-expiry:")
            .and_then(|value| value.strip_suffix("-->"))
            .map(str::trim)
        else {
            continue;
        };
        let value = Value::String(value.to_owned());
        return parse_expiry(&value, "marker expiry").ok()?.date();
    }
    None
}

fn repository_root(repo: &Path) -> Result<PathBuf> {
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve repository {}", repo.display()))?;
    ensure!(
        repo.is_dir(),
        "repository path {} is not a directory",
        repo.display()
    );
    Ok(repo)
}

fn target_path(repo: &Path, target: Option<&Path>) -> Result<PathBuf> {
    let target = target.unwrap_or_else(|| Path::new("AGENTS.md"));
    let path = if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo.join(target)
    };
    let existing_or_parent = if path.exists() {
        path.canonicalize()?
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("target {} has no parent", path.display()))?;
        parent.canonicalize()?.join(
            path.file_name()
                .ok_or_else(|| anyhow::anyhow!("target {} has no filename", path.display()))?,
        )
    };
    ensure!(
        existing_or_parent.starts_with(repo),
        "target {} must be inside repository {}",
        existing_or_parent.display(),
        repo.display()
    );
    Ok(existing_or_parent)
}

fn relative_target(repo: &Path, target: &Path) -> Result<String> {
    Ok(target
        .strip_prefix(repo)
        .with_context(|| format!("target {} is outside repository", target.display()))?
        .to_string_lossy()
        .into_owned())
}

fn receipt_path(repo: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(repo.join(PROMOTION_DIRECTORY).join(format!("{id}.json")))
}

fn read_target(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("read target {}", path.display()))
}

fn write_target(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create target directory {}", parent.display()))?;
    }
    write_atomic(path, content.as_bytes())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_vec_pretty(value).context("serialize promotion receipt")?;
    write_atomic(path, &content)
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path {} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create directory {}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.needle-tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&temp, content)
        .with_context(|| format!("write temporary file {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson_file(root: &Path, reviewed_by: &str) -> PathBuf {
        let path = root.join("candidate.md");
        fs::write(
            &path,
            format!(
                "---\nschema: needle.candidate-lesson/v1\nid: candidate-lesson-test\nfingerprint: fp-test\nevidence_refs:\n  - evidence.md\nscope:\n  repositories:\n    - fixture\nexpiry: 2099-01-01\nreviewed_by: {reviewed_by}\n---\n\n## Use the fixture gate\n\nRun the fixture gate before dispatch.\n"
            ),
        )
        .expect("write candidate lesson");
        fs::write(root.join("evidence.md"), "evidence").expect("write evidence");
        path
    }

    #[test]
    fn promote_and_demote_restore_target_bytes() {
        let root = tempfile::tempdir().expect("fixture root");
        let target = root.path().join("AGENTS.md");
        let original = "# Instructions\r\n\r\nKeep this exact.\r\n";
        fs::write(&target, original).expect("write target");
        let lesson = lesson_file(root.path(), "reviewer");

        promote(&lesson, root.path(), None).expect("promote lesson");
        let promoted = fs::read_to_string(&target).expect("read promoted target");
        assert!(promoted.contains("<!-- needle-lesson:candidate-lesson-test -->"));
        assert!(root
            .path()
            .join(PROMOTION_DIRECTORY)
            .join("candidate-lesson-test.json")
            .is_file());

        demote("candidate-lesson-test", root.path()).expect("demote lesson");
        assert_eq!(
            fs::read_to_string(target).expect("read restored target"),
            original
        );
    }

    #[test]
    fn unreviewed_lesson_is_refused() {
        let root = tempfile::tempdir().expect("fixture root");
        let lesson = lesson_file(root.path(), "");
        let error = promote(&lesson, root.path(), None).expect_err("unreviewed lesson must fail");
        assert!(error.to_string().contains("reviewed_by"));
        assert!(!root.path().join("AGENTS.md").exists());
    }

    #[test]
    fn expired_markers_are_reported() {
        let markers = expired_markers(
            "<!-- needle-lesson:old -->\n<!-- needle-lesson-expiry:2020-01-01T00:00:00Z -->\nold\n<!-- /needle-lesson:old -->",
            Utc::now(),
        );
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].id, "old");
    }
}
