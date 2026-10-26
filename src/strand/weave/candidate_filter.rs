//! Deterministic filters a Weave candidate must pass before it is filed.
//!
//! needle-ddf36d66: a read-only triage of 7,632 open generated proposals found
//! roughly half were near-duplicates of each other, and of 67 strong
//! candidates checked against the code, 36 were already implemented or stale.
//! The generator worked from documentation text rather than from the code at
//! HEAD. Two filters close that gap:
//!
//! - **Near-duplicate suppression.** A candidate whose normalized title and
//!   description are at least `duplicate_similarity_threshold` similar
//!   (Jaccard over distinctive word sets) to an open bead, to a bead closed
//!   within `duplicate_closed_lookback_days`, or to a candidate filed earlier
//!   in the same pass is dropped.
//! - **Evidence at HEAD.** Every candidate must cite evidence the checkout can
//!   confirm: a `file_line` whose quoted text is really on that line, a
//!   `missing_artifact` that really is absent, or a `failing_command` with its
//!   observed output. At least one item must point at something other than
//!   the documentation Weave was given — a citation of the plan file counts,
//!   it is the rank-2 source ([`super::source_priority`]) — and every cited
//!   item must verify. A candidate that fails either rule is dropped, never
//!   filed.
//! - **Source priority** (needle-21c5e0a3). A generic "add tests" or
//!   "improve documentation" candidate that cites no dated defect (a failing
//!   command, or a citation in code a recent commit touched) and no plan
//!   section is dropped as `generic_no_defect`. Every admissible candidate
//!   is classified into the ranked source it draws from — dated defect,
//!   plan gap, or bare code citation at HEAD ([`CandidateSource`]).
//!
//! All three filters are pure functions of the candidate, the workspace
//! inventory, and the files on disk, so the same inputs always give the same
//! verdict.

use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Duration, Utc};

use super::source_priority;
use crate::types::{Bead, BeadStatus};

/// Heading under which verified evidence is appended to a filed bead body.
pub(crate) const EVIDENCE_HEADING: &str = "## Evidence at HEAD";
/// Start of the provenance footer Weave appends to every bead it files.
const WEAVE_FOOTER_MARKER: &str = "\n---\nPriority:";

/// Telemetry reason: near-duplicate of an existing or just-filed bead.
pub(crate) const REASON_DUPLICATE: &str = "duplicate";
/// Telemetry reason: no evidence, or only documentation citations.
pub(crate) const REASON_NO_EVIDENCE: &str = "no_evidence";
/// Telemetry reason: a cited item is malformed or the checkout contradicts it.
pub(crate) const REASON_EVIDENCE_UNVERIFIED: &str = "evidence_unverified";
/// Telemetry reason: a generic documentation or test/coverage proposal that
/// cites no dated defect and no plan gap (needle-21c5e0a3).
pub(crate) const REASON_GENERIC_NO_DEFECT: &str = "generic_no_defect";

/// A cited file larger than this is not read; the citation does not verify.
const MAX_EVIDENCE_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// More items than this is treated as malformed rather than read one by one.
const MAX_EVIDENCE_ITEMS: usize = 12;
/// A quote shorter than this (after whitespace normalization) proves nothing:
/// `}` or `ok` appears on almost any line.
const MIN_QUOTE_CHARS: usize = 5;
/// Lines either side of the cited line (or range) searched for the quote, so
/// an off-by-one citation of a real line still verifies.
const QUOTE_LINE_TOLERANCE: usize = 2;
/// A cited line range wider than this is narrowed to its first lines.
const MAX_QUOTE_WINDOW_LINES: usize = 200;
/// Bounds on text copied from the response into the filed bead body.
const MAX_RENDERED_QUOTE_CHARS: usize = 200;
const MAX_RENDERED_OBSERVED_CHARS: usize = 600;

/// Words too common in bead text to say anything about which gap it names.
/// Calibrated with the default threshold against the 2026-10-16 inventories
/// of bead-rs, mta-my-way and NEEDLE (see `docs/configuration.md`).
const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "but",
    "by",
    "can",
    "could",
    "does",
    "doing",
    "done",
    "each",
    "for",
    "from",
    "has",
    "have",
    "how",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "may",
    "might",
    "must",
    "no",
    "not",
    "of",
    "on",
    "or",
    "our",
    "should",
    "so",
    "such",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "this",
    "those",
    "to",
    "too",
    "under",
    "up",
    "use",
    "used",
    "using",
    "via",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "will",
    "with",
    "within",
    "without",
    "would",
    "you",
    "your",
    "also",
    "all",
    "any",
    "new",
    "add",
    "adds",
    "added",
    "ensure",
    "make",
    "makes",
    "implement",
    "implements",
    "implementation",
    "support",
    "supports",
];

/// Why a candidate was not filed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CandidateRejection {
    /// One of the `REASON_*` telemetry reasons.
    pub reason: &'static str,
    /// Human-readable specifics, e.g. which citation failed and why.
    pub detail: String,
}

impl CandidateRejection {
    fn no_evidence(detail: impl Into<String>) -> Self {
        Self {
            reason: REASON_NO_EVIDENCE,
            detail: detail.into(),
        }
    }

    fn unverified(detail: impl Into<String>) -> Self {
        Self {
            reason: REASON_EVIDENCE_UNVERIFIED,
            detail: detail.into(),
        }
    }
}

// ─── Near-duplicate suppression ─────────────────────────────────────────────

/// The distinctive words of a title and description.
///
/// Lower-cased, split on every non-ASCII-alphanumeric character, with words
/// shorter than three characters and [`STOPWORDS`] removed. The description is
/// cut at Weave's own evidence section and provenance footer, so boilerplate
/// every generated bead shares cannot make two unrelated beads look alike.
pub(crate) fn similarity_tokens(title: &str, body: &str) -> BTreeSet<String> {
    let evidence_marker = format!("\n{EVIDENCE_HEADING}");
    let mut end = body.len();
    for marker in [WEAVE_FOOTER_MARKER, evidence_marker.as_str()] {
        if let Some(index) = body.find(marker) {
            end = end.min(index);
        }
    }
    let text = format!("{title} {}", &body[..end]).to_lowercase();
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() >= 3 && !STOPWORDS.contains(word))
        .map(str::to_string)
        .collect()
}

/// Jaccard similarity of two word sets: shared words over all words.
pub(crate) fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// The beads a candidate must not duplicate, as word sets.
pub(crate) struct DuplicateIndex {
    entries: Vec<(String, BTreeSet<String>)>,
}

impl DuplicateIndex {
    /// Every bead that is not closed, plus beads closed within
    /// `closed_lookback_days` of `now`. bead-rs list output carries no
    /// `closed_at`, so a closed bead's `updated_at` stands for its close time;
    /// a later edit can only keep it in the window longer, never shorter.
    pub(crate) fn from_inventory(
        beads: &[Bead],
        now: DateTime<Utc>,
        closed_lookback_days: u32,
    ) -> Self {
        let cutoff = now - Duration::days(i64::from(closed_lookback_days));
        let entries = beads
            .iter()
            .filter(|bead| {
                bead.status != BeadStatus::Closed
                    || (closed_lookback_days > 0 && bead.updated_at >= cutoff)
            })
            .map(|bead| {
                (
                    bead.id.to_string(),
                    similarity_tokens(&bead.title, bead.body.as_deref().unwrap_or("")),
                )
            })
            .collect();
        Self { entries }
    }

    /// The most similar indexed bead, when it reaches `threshold`.
    ///
    /// A threshold of zero (or below) disables the check. Ties keep the bead
    /// listed first, so the verdict does not depend on iteration order.
    pub(crate) fn best_match(
        &self,
        tokens: &BTreeSet<String>,
        threshold: f64,
    ) -> Option<(String, f64)> {
        if threshold <= 0.0 || tokens.is_empty() {
            return None;
        }
        let mut best: Option<(&str, f64)> = None;
        for (id, entry) in &self.entries {
            let similarity = jaccard(tokens, entry);
            if best.is_none_or(|(_, current)| similarity > current) {
                best = Some((id, similarity));
            }
        }
        best.filter(|(_, similarity)| *similarity >= threshold)
            .map(|(id, similarity)| (id.to_string(), similarity))
    }

    /// Index a bead filed during this pass, so a later candidate in the same
    /// response cannot file the same gap twice.
    pub(crate) fn insert(&mut self, id: String, tokens: BTreeSet<String>) {
        self.entries.push((id, tokens));
    }
}

// ─── Evidence at HEAD ───────────────────────────────────────────────────────

/// One evidence item the checkout confirmed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VerifiedEvidence {
    /// The quoted text is on (or within two lines of) the cited line.
    FileLine {
        path: String,
        line: usize,
        quote: String,
        /// The file is one of the documentation files Weave was shown. Such a
        /// citation supports a candidate but never qualifies it on its own.
        documentation: bool,
    },
    /// The path does not exist in the checkout.
    MissingArtifact { path: String },
    /// A command the agent reports failing, with what it observed. NEEDLE
    /// does not re-run model-supplied commands, so this is checked for shape
    /// only.
    FailingCommand { command: String, observed: String },
}

impl VerifiedEvidence {
    /// Whether this item shows a gap in the code rather than in the docs.
    ///
    /// A line of the plan file is the exception: it is the rank-2 source, so
    /// a plan citation qualifies a candidate on its own.
    fn qualifies(&self) -> bool {
        match self {
            VerifiedEvidence::FileLine {
                path,
                documentation,
                ..
            } => !documentation || source_priority::is_plan_file(path),
            VerifiedEvidence::MissingArtifact { .. } => true,
            VerifiedEvidence::FailingCommand { .. } => true,
        }
    }
}

// ─── Source priority (needle-21c5e0a3) ──────────────────────────────────────

/// Where an admissible candidate draws from, in Weave's priority order.
///
/// The ordering is the ranking: dated defects file before plan gaps, plan
/// gaps before bare code citations at HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CandidateSource {
    /// Rank 1: tied to a dated defect — a failing command with observed
    /// output, or a citation in code a recent commit touched.
    DatedDefect,
    /// Rank 2: cites the plan file; the gap is between
    /// `docs/plan/plan.md` and what is built.
    PlanGap,
    /// Neither: a candidate that cites code at HEAD and is not a generic
    /// documentation or test/coverage proposal. This is the pre-existing
    /// evidence bar from needle-ddf36d66, filed last.
    CodeAtHead,
}

/// The generic flavors a candidate must not carry without a defect or plan
/// reference.
#[derive(Debug, Clone, Copy)]
enum GenericFlavor {
    TestCoverage,
    Documentation,
}

impl GenericFlavor {
    fn label(&self) -> &'static str {
        match self {
            GenericFlavor::TestCoverage => "test/coverage",
            GenericFlavor::Documentation => "documentation",
        }
    }
}

/// Titles too generic to file: test/coverage or documentation work.
///
/// Judged on the title only — a defect bead may legitimately promise tests
/// or a doc touch in its body without the candidate *being* that kind of
/// filler. A title carrying these words still files when its evidence cites
/// a dated defect or a plan section; the flavor only decides bare proposals.
fn generic_flavor(title: &str) -> Option<GenericFlavor> {
    const TEST_COVERAGE_WORDS: &[&str] = &[
        "test",
        "tests",
        "testing",
        "testable",
        "untested",
        "uncovered",
        "unittest",
        "pytest",
        "nextest",
        "coverage",
    ];
    const DOCUMENTATION_WORDS: &[&str] = &[
        "doc",
        "docs",
        "document",
        "documenting",
        "documented",
        "documentation",
        "readme",
        "changelog",
    ];
    let lowercase = title.to_lowercase();
    let words: HashSet<&str> = lowercase
        .split(|c: char| !c.is_ascii_alphanumeric())
        .collect();
    if words.iter().any(|word| TEST_COVERAGE_WORDS.contains(word)) {
        Some(GenericFlavor::TestCoverage)
    } else if words.iter().any(|word| DOCUMENTATION_WORDS.contains(word)) {
        Some(GenericFlavor::Documentation)
    } else {
        None
    }
}

/// Classify a verified candidate against the ranked sources.
///
/// `verify_evidence` has already confirmed every cited item; this decides
/// *which source* the candidate draws from and whether that source is
/// admissible. A generic test/coverage or documentation candidate with no
/// dated defect (failing command, or a citation in recently changed code)
/// and no plan citation is rejected — measured across 6,937 proposals, that
/// is the class that flooded the queue (needle-21c5e0a3).
pub(crate) fn classify_candidate(
    title: &str,
    verified: &[VerifiedEvidence],
    inventory: &source_priority::SourceInventory,
) -> Result<CandidateSource, CandidateRejection> {
    let plan_gap = verified.iter().any(|item| {
        matches!(item, VerifiedEvidence::FileLine { path, .. }
            if source_priority::is_plan_file(path))
    });
    let dated_defect = verified.iter().any(|item| match item {
        VerifiedEvidence::FailingCommand { .. } => true,
        VerifiedEvidence::FileLine {
            path,
            documentation,
            ..
        } => !*documentation && inventory.contains_recent_change(path),
        VerifiedEvidence::MissingArtifact { path } => inventory.contains_recent_change(path),
    });

    if dated_defect {
        return Ok(CandidateSource::DatedDefect);
    }
    if plan_gap {
        return Ok(CandidateSource::PlanGap);
    }
    if let Some(flavor) = generic_flavor(title) {
        return Err(CandidateRejection {
            reason: REASON_GENERIC_NO_DEFECT,
            detail: format!(
                "generic {} proposal cites no dated defect or plan gap; \
                 only a failing command, a citation in recently changed code, \
                 or a {} section qualifies",
                flavor.label(),
                source_priority::PLAN_FILE
            ),
        });
    }
    Ok(CandidateSource::CodeAtHead)
}

/// Verify a candidate's `evidence` against the workspace checkout.
///
/// Accepts a list of evidence objects (a single object is treated as a list
/// of one). Every item must verify; at least one must be a `file_line` in a
/// non-documentation file, a `missing_artifact`, or a `failing_command`.
pub(crate) fn verify_evidence(
    evidence: &serde_json::Value,
    workspace: &Path,
    doc_patterns: &[String],
) -> Result<Vec<VerifiedEvidence>, CandidateRejection> {
    let items: Vec<&serde_json::Value> = match evidence {
        serde_json::Value::Null => {
            return Err(CandidateRejection::no_evidence(
                "candidate cites no evidence at HEAD",
            ))
        }
        serde_json::Value::Array(items) if items.is_empty() => {
            return Err(CandidateRejection::no_evidence(
                "candidate cites no evidence at HEAD",
            ))
        }
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![evidence],
        _ => {
            return Err(CandidateRejection::unverified(
                "evidence must be a list of objects with a kind",
            ))
        }
    };
    if items.len() > MAX_EVIDENCE_ITEMS {
        return Err(CandidateRejection::unverified(format!(
            "{} evidence items exceeds the limit of {MAX_EVIDENCE_ITEMS}",
            items.len()
        )));
    }

    let canonical_workspace = workspace.canonicalize().map_err(|error| {
        CandidateRejection::unverified(format!(
            "workspace {} cannot be resolved: {error}",
            workspace.display()
        ))
    })?;

    let mut verified = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let verdict = verify_item(item, &canonical_workspace, doc_patterns).map_err(|detail| {
            CandidateRejection::unverified(format!("evidence item {}: {detail}", index + 1))
        })?;
        verified.push(verdict);
    }

    if !verified.iter().any(VerifiedEvidence::qualifies) {
        return Err(CandidateRejection::no_evidence(
            "every citation is a documentation line; a gap needs code, a missing artifact, or a failing command",
        ));
    }
    Ok(verified)
}

fn verify_item(
    item: &serde_json::Value,
    workspace: &Path,
    doc_patterns: &[String],
) -> Result<VerifiedEvidence, String> {
    let object = item
        .as_object()
        .ok_or_else(|| "evidence item is not an object".to_string())?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let kind = text("kind")
        .ok_or_else(|| "evidence item has no kind".to_string())?
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");

    match kind.as_str() {
        "file_line" => {
            let (raw_path, start, end) = match text("location") {
                Some(location) => split_location(location)
                    .ok_or_else(|| format!("location '{location}' is not path:line"))?,
                None => {
                    let path = text("path")
                        .ok_or_else(|| "file_line needs a location (path:line)".to_string())?;
                    let line = object
                        .get("line")
                        .and_then(|value| {
                            value
                                .as_u64()
                                .map(|line| line as usize)
                                .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
                        })
                        .ok_or_else(|| "file_line needs a line number".to_string())?;
                    (path, line, line)
                }
            };
            let quote = text("quote")
                .ok_or_else(|| "file_line needs a quote of the cited line".to_string())?;
            let quote = normalize_whitespace(quote);
            if quote.chars().count() < MIN_QUOTE_CHARS
                || !quote.chars().any(char::is_alphanumeric)
            {
                return Err(format!(
                    "quote '{quote}' is too short to identify the cited line"
                ));
            }
            let relative = relative_path(raw_path, workspace)?;
            let lines = read_lines(workspace, &relative)?;
            if start == 0 || start > lines.len() {
                return Err(format!(
                    "{}:{start} is outside the file ({} lines)",
                    relative.display(),
                    lines.len()
                ));
            }
            let end = end
                .max(start)
                .min(start + MAX_QUOTE_WINDOW_LINES)
                .min(lines.len());
            let first = start.saturating_sub(1 + QUOTE_LINE_TOLERANCE);
            let last = (end + QUOTE_LINE_TOLERANCE).min(lines.len());
            let window = normalize_whitespace(&lines[first..last].join(" "));
            if !window.contains(&quote) {
                return Err(format!(
                    "quote is not at {}:{start} in the checkout",
                    relative.display()
                ));
            }
            Ok(VerifiedEvidence::FileLine {
                documentation: is_documentation(&relative, doc_patterns),
                path: relative.display().to_string(),
                line: start,
                quote,
            })
        }
        "missing_artifact" => {
            let raw_path = text("path")
                .or_else(|| text("location"))
                .ok_or_else(|| "missing_artifact needs a path".to_string())?;
            let relative = relative_path(raw_path, workspace)?;
            match workspace.join(&relative).symlink_metadata() {
                Ok(_) => Err(format!(
                    "{} is reported missing but exists in the checkout",
                    relative.display()
                )),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(VerifiedEvidence::MissingArtifact {
                        path: relative.display().to_string(),
                    })
                }
                Err(error) => Err(format!("{} cannot be checked: {error}", relative.display())),
            }
        }
        "failing_command" => {
            let command = text("command")
                .ok_or_else(|| "failing_command needs the command".to_string())?;
            let observed = text("observed")
                .ok_or_else(|| "failing_command needs the observed failure".to_string())?;
            Ok(VerifiedEvidence::FailingCommand {
                command: command.to_string(),
                observed: observed.to_string(),
            })
        }
        other => Err(format!(
            "unknown evidence kind '{other}' (expected file_line, missing_artifact or failing_command)"
        )),
    }
}

/// Split `path:line`, `path:line:column` or `path:start-end`.
fn split_location(location: &str) -> Option<(&str, usize, usize)> {
    let parse_range = |value: &str| -> Option<(usize, usize)> {
        let value = value.trim().trim_start_matches(['L', 'l']);
        match value.split_once('-') {
            Some((start, end)) => {
                let start = start.trim().parse().ok()?;
                let end = end.trim().trim_start_matches(['L', 'l']).parse().ok()?;
                Some((start, end))
            }
            None => value.parse().ok().map(|line| (line, line)),
        }
    };
    let (head, tail) = location.rsplit_once(':')?;
    // `path:line:column` — the tail is a column, the line precedes it.
    if let Some((path, line)) = head.rsplit_once(':') {
        if let (Ok(line), Ok(_column)) =
            (line.trim().parse::<usize>(), tail.trim().parse::<usize>())
        {
            return Some((path, line, line));
        }
    }
    let (start, end) = parse_range(tail)?;
    Some((head, start, end))
}

/// Resolve a cited path to a clean path relative to the workspace.
///
/// An absolute path inside the workspace is accepted and made relative. A
/// path that leaves the workspace, or points into `.git` or `.beads`, is not
/// evidence about the code at HEAD.
fn relative_path(raw: &str, workspace: &Path) -> Result<PathBuf, String> {
    let raw = raw.trim().trim_matches('`');
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.strip_prefix(workspace)
            .map_err(|_| format!("{raw} is outside the workspace"))?
    } else {
        path
    };
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{raw} escapes the workspace"))
            }
        }
    }
    let first = relative
        .components()
        .next()
        .ok_or_else(|| format!("'{raw}' names no path"))?;
    if matches!(first, Component::Normal(part) if part == ".git" || part == ".beads") {
        return Err(format!("{raw} is repository metadata, not code"));
    }
    Ok(relative)
}

/// Read a cited file's lines, refusing anything that resolves outside the
/// workspace (a symlink) or is too large to be source.
fn read_lines(workspace: &Path, relative: &Path) -> Result<Vec<String>, String> {
    let path = workspace.join(relative);
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("{} does not exist in the checkout", relative.display()))?;
    if !canonical.starts_with(workspace) {
        return Err(format!(
            "{} resolves outside the workspace",
            relative.display()
        ));
    }
    let metadata = canonical
        .metadata()
        .map_err(|error| format!("{} cannot be read: {error}", relative.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", relative.display()));
    }
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        return Err(format!("{} is too large to verify", relative.display()));
    }
    let bytes = std::fs::read(&canonical)
        .map_err(|error| format!("{} cannot be read: {error}", relative.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_string)
        .collect())
}

fn is_documentation(relative: &Path, doc_patterns: &[String]) -> bool {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    doc_patterns.iter().any(|pattern| {
        glob::Pattern::new(pattern)
            .map(|pattern| pattern.matches_path_with(relative, options))
            .unwrap_or(false)
    })
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render verified evidence as the section appended to a filed bead body, so
/// whoever admits or implements the bead sees what proved the gap.
pub(crate) fn render_evidence(items: &[VerifiedEvidence]) -> String {
    let mut output = String::from(EVIDENCE_HEADING);
    for item in items {
        let line = match item {
            VerifiedEvidence::FileLine {
                path, line, quote, ..
            } => format!(
                "- `{path}:{line}`: `{}`",
                inline(quote, MAX_RENDERED_QUOTE_CHARS)
            ),
            VerifiedEvidence::MissingArtifact { path } => {
                format!("- missing artifact: `{path}`")
            }
            VerifiedEvidence::FailingCommand { command, observed } => format!(
                "- failing command: `{}` (observed: {})",
                inline(command, MAX_RENDERED_QUOTE_CHARS),
                inline(observed, MAX_RENDERED_OBSERVED_CHARS)
            ),
        };
        output.push('\n');
        output.push_str(&line);
    }
    output
}

/// One line of bounded, backtick-free text.
fn inline(value: &str, max_chars: usize) -> String {
    let flat = normalize_whitespace(value).replace('`', "'");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut bounded: String = flat.chars().take(max_chars).collect();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BeadId;
    use serde_json::json;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("src/auth.rs"),
            "pub fn login() {}\n\npub fn refresh_token() {\n    todo!(\"refresh tokens\")\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docs/plan.md"),
            "# Plan\n\nToken refresh rotates the session.\n",
        )
        .unwrap();
        dir
    }

    fn doc_patterns() -> Vec<String> {
        vec!["README*".to_string(), "docs/**/*".to_string()]
    }

    fn bead(id: &str, title: &str, body: &str, status: BeadStatus, age_days: i64) -> Bead {
        let at = Utc::now() - Duration::days(age_days);
        Bead {
            id: BeadId::from(id),
            title: title.to_string(),
            body: Some(body.to_string()),
            priority: 2,
            status,
            assignee: None,
            labels: Vec::new(),
            workspace: PathBuf::from("/workspace"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn rephrased_proposal_is_a_duplicate_of_the_open_one() {
        // Two of the mta-my-way auth-cluster proposals from 2026-10-16.
        let open = bead(
            "mta-1",
            "Require authentication on public trip and journal read endpoints",
            "GET /api/trips and /api/journal return user data without authentication.\n\n---\nPriority: P1\nCreated by: weave strand",
            BeadStatus::Open,
            3,
        );
        let index = DuplicateIndex::from_inventory(&[open], Utc::now(), 30);
        let candidate = similarity_tokens(
            "Enforce authentication on public trips and journal GET endpoints",
            "The trips and journal read endpoints return user data without authentication.",
        );

        let (matched, similarity) = index
            .best_match(&candidate, 0.35)
            .expect("a rephrased proposal must be caught");
        assert_eq!(matched, "mta-1");
        assert!(similarity >= 0.35, "similarity {similarity}");
    }

    #[test]
    fn distinct_gaps_are_not_duplicates() {
        let open = bead(
            "ws-1",
            "Require authentication on public trip and journal read endpoints",
            "GET /api/trips returns user data without authentication.",
            BeadStatus::Open,
            1,
        );
        let index = DuplicateIndex::from_inventory(&[open], Utc::now(), 30);
        let candidate = similarity_tokens(
            "Migrate .eslintignore into eslint.config.js ignores",
            "ESLint warns that .eslintignore is deprecated on every CI run.",
        );
        assert_eq!(index.best_match(&candidate, 0.35), None);
    }

    #[test]
    fn closed_beads_count_only_inside_the_lookback_window() {
        let title = "Retire the dead /push/ and /auth/ IngressRoute rules";
        let body = "The IngressRoute still routes /push/ and /auth/ to the retired service.";
        let recent = bead("recent", title, body, BeadStatus::Closed, 5);
        let old = bead("old", title, body, BeadStatus::Closed, 45);
        let candidate = similarity_tokens(title, body);

        let index = DuplicateIndex::from_inventory(std::slice::from_ref(&old), Utc::now(), 30);
        assert_eq!(index.best_match(&candidate, 0.35), None);

        let index = DuplicateIndex::from_inventory(&[old.clone(), recent], Utc::now(), 30);
        assert_eq!(
            index.best_match(&candidate, 0.35).map(|(id, _)| id),
            Some("recent".to_string())
        );

        // A zero lookback compares against beads that are not closed only.
        let index = DuplicateIndex::from_inventory(&[old], Utc::now(), 0);
        assert_eq!(index.best_match(&candidate, 0.35), None);
    }

    #[test]
    fn zero_threshold_disables_duplicate_suppression() {
        let title = "Retire the dead /push/ IngressRoute rules";
        let open = bead("ws-1", title, "", BeadStatus::Open, 1);
        let index = DuplicateIndex::from_inventory(&[open], Utc::now(), 30);
        assert_eq!(index.best_match(&similarity_tokens(title, ""), 0.0), None);
    }

    #[test]
    fn weave_boilerplate_does_not_make_unrelated_beads_similar() {
        let a = similarity_tokens(
            "Rotate refresh tokens",
            "Body.\n\n## Evidence at HEAD\n- `src/auth.rs:4`: `todo!(\"refresh tokens\")`\n\n---\nPriority: P2\nCreated by: weave strand",
        );
        assert!(!a.contains("weave"), "footer leaked into tokens: {a:?}");
        assert!(
            !a.contains("evidence"),
            "evidence leaked into tokens: {a:?}"
        );
        assert_eq!(
            similarity_tokens("Same title words", "x\n---\nPriority: P1"),
            similarity_tokens("Same title words", "")
        );
    }

    #[test]
    fn file_line_evidence_verifies_against_the_checkout() {
        let dir = workspace();
        let evidence = json!([
            {"kind": "file_line", "location": "src/auth.rs:4", "quote": "todo!(\"refresh tokens\")"}
        ]);
        let verified = verify_evidence(&evidence, dir.path(), &doc_patterns()).unwrap();
        assert_eq!(
            verified,
            vec![VerifiedEvidence::FileLine {
                path: "src/auth.rs".to_string(),
                line: 4,
                quote: "todo!(\"refresh tokens\")".to_string(),
                documentation: false,
            }]
        );

        // Off by one line, a path:line:column location, and an absolute
        // path inside the workspace all still identify the same real line.
        let absolute = format!(
            "{}:3",
            dir.path()
                .canonicalize()
                .unwrap()
                .join("src/auth.rs")
                .display()
        );
        for location in ["src/auth.rs:5", "src/auth.rs:4:5", absolute.as_str()] {
            let evidence = json!([{"kind": "file_line", "location": location, "quote": "todo!(\"refresh tokens\")"}]);
            assert!(
                verify_evidence(&evidence, dir.path(), &doc_patterns()).is_ok(),
                "{location} should verify"
            );
        }
    }

    #[test]
    fn fabricated_or_unsafe_file_citations_are_rejected() {
        let dir = workspace();
        let cases = [
            json!({"kind": "file_line", "location": "src/auth.rs:4", "quote": "fn rotate_session()"}),
            json!({"kind": "file_line", "location": "src/auth.rs:40", "quote": "todo!(\"refresh tokens\")"}),
            json!({"kind": "file_line", "location": "src/missing.rs:1", "quote": "pub fn login"}),
            json!({"kind": "file_line", "location": "../outside.rs:1", "quote": "pub fn login"}),
            json!({"kind": "file_line", "location": ".beads/issues.jsonl:1", "quote": "pub fn login"}),
            json!({"kind": "file_line", "location": "src/auth.rs:1", "quote": "}"}),
            json!({"kind": "file_line", "location": "src/auth.rs:1"}),
            json!({"kind": "vibes", "note": "it feels incomplete"}),
            json!("src/auth.rs:4"),
        ];
        for item in cases {
            let rejection = verify_evidence(&json!([item.clone()]), dir.path(), &doc_patterns())
                .expect_err(&format!("{item} must not verify"));
            assert_eq!(rejection.reason, REASON_EVIDENCE_UNVERIFIED, "{item}");
        }
    }

    #[test]
    fn missing_artifact_must_really_be_missing() {
        let dir = workspace();
        let verified = verify_evidence(
            &json!([{"kind": "missing_artifact", "path": "tests/auth_refresh.rs"}]),
            dir.path(),
            &doc_patterns(),
        )
        .unwrap();
        assert_eq!(
            verified,
            vec![VerifiedEvidence::MissingArtifact {
                path: "tests/auth_refresh.rs".to_string()
            }]
        );

        // The already-implemented class: the "missing" file is at HEAD.
        let rejection = verify_evidence(
            &json!([{"kind": "missing_artifact", "path": "src/auth.rs"}]),
            dir.path(),
            &doc_patterns(),
        )
        .unwrap_err();
        assert_eq!(rejection.reason, REASON_EVIDENCE_UNVERIFIED);
        assert!(rejection.detail.contains("exists"), "{}", rejection.detail);
    }

    #[test]
    fn failing_command_needs_command_and_observed_output() {
        let dir = workspace();
        assert!(verify_evidence(
            &json!([{"kind": "failing-command", "command": "cargo test auth::refresh", "observed": "panicked at 'not yet implemented: refresh tokens'"}]),
            dir.path(),
            &doc_patterns(),
        )
        .is_ok());
        let rejection = verify_evidence(
            &json!([{"kind": "failing_command", "command": "cargo test"}]),
            dir.path(),
            &doc_patterns(),
        )
        .unwrap_err();
        assert_eq!(rejection.reason, REASON_EVIDENCE_UNVERIFIED);
    }

    #[test]
    fn documentation_citations_alone_do_not_qualify() {
        let dir = workspace();
        let doc_only = json!([
            {"kind": "file_line", "location": "docs/plan.md:3", "quote": "Token refresh rotates the session."}
        ]);
        let rejection = verify_evidence(&doc_only, dir.path(), &doc_patterns()).unwrap_err();
        assert_eq!(rejection.reason, REASON_NO_EVIDENCE);

        // The same doc citation is fine beside code evidence.
        let mixed = json!([
            {"kind": "file_line", "location": "docs/plan.md:3", "quote": "Token refresh rotates the session."},
            {"kind": "file_line", "location": "src/auth.rs:4", "quote": "todo!(\"refresh tokens\")"}
        ]);
        assert_eq!(
            verify_evidence(&mixed, dir.path(), &doc_patterns())
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn absent_evidence_is_no_evidence_and_one_bad_item_sinks_the_candidate() {
        let dir = workspace();
        for evidence in [json!(null), json!([])] {
            assert_eq!(
                verify_evidence(&evidence, dir.path(), &doc_patterns())
                    .unwrap_err()
                    .reason,
                REASON_NO_EVIDENCE
            );
        }
        let one_fabricated = json!([
            {"kind": "missing_artifact", "path": "tests/auth_refresh.rs"},
            {"kind": "file_line", "location": "src/auth.rs:2", "quote": "fn rotate_session()"}
        ]);
        assert_eq!(
            verify_evidence(&one_fabricated, dir.path(), &doc_patterns())
                .unwrap_err()
                .reason,
            REASON_EVIDENCE_UNVERIFIED
        );
    }

    #[test]
    fn rendered_evidence_is_bounded_and_backtick_safe() {
        let rendered = render_evidence(&[
            VerifiedEvidence::FileLine {
                path: "src/auth.rs".to_string(),
                line: 4,
                quote: "todo!(`x`)".to_string(),
                documentation: false,
            },
            VerifiedEvidence::MissingArtifact {
                path: "tests/auth_refresh.rs".to_string(),
            },
            VerifiedEvidence::FailingCommand {
                command: "cargo test".to_string(),
                observed: "x".repeat(2_000),
            },
        ]);
        assert!(rendered.starts_with(EVIDENCE_HEADING));
        assert!(rendered.contains("- `src/auth.rs:4`: `todo!('x')`"));
        assert!(rendered.contains("- missing artifact: `tests/auth_refresh.rs`"));
        assert!(rendered.len() < 1_000, "observed output must be bounded");
    }

    fn inventory_with_recent_change() -> source_priority::SourceInventory {
        source_priority::SourceInventory::fixture(&["src/lib.rs"])
    }

    #[test]
    fn a_dated_defect_files_even_with_a_generic_title() {
        let inventory = inventory_with_recent_change();
        let failing = [VerifiedEvidence::FailingCommand {
            command: "cargo test".to_string(),
            observed: "1 failed".to_string(),
        }];
        assert!(matches!(
            classify_candidate("Add more tests", &failing, &inventory),
            Ok(CandidateSource::DatedDefect)
        ));
        let cited = [VerifiedEvidence::FileLine {
            path: "src/lib.rs".to_string(),
            line: 1,
            quote: "fn main".to_string(),
            documentation: false,
        }];
        assert!(matches!(
            classify_candidate("Handle the empty input", &cited, &inventory),
            Ok(CandidateSource::DatedDefect)
        ));
        // A missing artifact is dated when a recent commit touched its path.
        let missing = [VerifiedEvidence::MissingArtifact {
            path: "src/lib.rs".to_string(),
        }];
        assert!(matches!(
            classify_candidate("Add more tests", &missing, &inventory),
            Ok(CandidateSource::DatedDefect)
        ));
    }

    #[test]
    fn a_plan_citation_is_the_rank_2_source_and_beats_the_generic_rejection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/plan")).unwrap();
        std::fs::write(dir.path().join("docs/plan/plan.md"), "# Plan\n").unwrap();
        let inventory = source_priority::SourceInventory::discover(dir.path());
        // doc_patterns cover docs/**/*, so this citation is documentation;
        // the plan-file exception is what qualifies it.
        let plan_line = [VerifiedEvidence::FileLine {
            path: "docs/plan/plan.md".to_string(),
            line: 1,
            quote: "# Plan".to_string(),
            documentation: true,
        }];
        assert!(matches!(
            classify_candidate("Add more tests", &plan_line, &inventory),
            Ok(CandidateSource::PlanGap)
        ));
    }

    #[test]
    fn a_generic_title_with_no_defect_or_plan_citation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = source_priority::SourceInventory::discover(dir.path());
        let bare_code = [VerifiedEvidence::FileLine {
            path: "src/lib.rs".to_string(),
            line: 1,
            quote: "fn main".to_string(),
            documentation: false,
        }];
        let rejection = classify_candidate("Add more tests", &bare_code, &inventory)
            .expect_err("a bare generic proposal must be rejected");
        assert_eq!(rejection.reason, REASON_GENERIC_NO_DEFECT);
    }
}
