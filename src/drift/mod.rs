//! Drift detection across worker sessions.
//!
//! Drift detection compares how similar problems are solved across multiple
//! sessions, identifying divergent approaches that may indicate either:
//! - Evolved solutions (improving over time)
//! - Inconsistent solutions (no clear progression, potential standardization opportunity)
//!
//! ## Detection Process
//!
//! 1. **Fingerprinting**: Extract key characteristics from each session transcript
//!    - Bead type and labels
//!    - Tool usage patterns
//!    - File patterns touched
//!    - Task description keywords
//!
//! 2. **Clustering**: Group similar sessions using Jaccard similarity on fingerprints
//!
//! 3. **Comparison**: Within each cluster, compare approaches across sessions
//!    - Different tool choices for similar tasks
//!    - Different file access patterns
//!    - Different action sequences
//!
//! 4. **Categorization**: Classify drift as "evolved" or "inconsistent"
//!    - Evolved: Clear temporal progression (later sessions consistently use approach B over A)
//!    - Inconsistent: No clear pattern (random alternation between approaches)
//!
//! ## Output
//!
//! Drift reports are written to `.beads/drifts/` and fed back into the
//! learning consolidation pipeline as high-signal inputs.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::learning::{BeadType, Confidence, LearningEntry};
use crate::transcript::{ParsedTranscript, ActionType};

// ──────────────────────────────────────────────────────────────────────────────
// Session Fingerprint
// ──────────────────────────────────────────────────────────────────────────────

/// A compact representation of a session's approach for similarity comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFingerprint {
    /// Session ID from transcript filename.
    pub session_id: String,
    /// When the session was last modified.
    pub modified_at: DateTime<Utc>,
    /// Bead ID if referenced in the session.
    pub bead_id: Option<String>,
    /// Type of bead work.
    pub bead_type: Option<BeadType>,
    /// Normalized keywords from task description.
    pub task_keywords: HashSet<String>,
    /// Tools used in this session (e.g., "Read", "Bash", "Edit").
    pub tools_used: HashSet<String>,
    /// File patterns touched (e.g., "src/*.rs", "Cargo.toml").
    pub file_patterns: HashSet<String>,
    /// Count of actions by type.
    pub action_counts: HashMap<String, usize>,
}

impl SessionFingerprint {
    /// Create a fingerprint from a parsed transcript.
    pub fn from_transcript(transcript: &ParsedTranscript) -> Self {
        let mut tools_used = HashSet::new();
        let mut file_patterns = HashSet::new();
        let mut action_counts = HashMap::new();

        // Extract tools, file patterns, and action counts
        for action in &transcript.actions {
            // Track tool usage
            if let Some(ref tool) = action.tool_name {
                tools_used.insert(tool.clone());
                *action_counts.entry(tool.clone()).or_insert(0) += 1;
            }

            // Track file patterns from tool inputs
            if let Some(pattern) = Self::extract_file_pattern(&action.description) {
                file_patterns.insert(pattern);
            }

            // Count action types
            let type_key = match action.action_type {
                ActionType::Text => "text",
                ActionType::ToolUse => action.tool_name.as_deref().unwrap_or("unknown_tool"),
                ActionType::Thinking => "thinking",
            };
            *action_counts.entry(type_key.to_string()).or_insert(0) += 1;
        }

        // Extract keywords from task description
        let task_keywords = Self::extract_keywords(
            transcript.task_description.as_deref().unwrap_or("")
        );

        SessionFingerprint {
            session_id: transcript.session_id.clone(),
            modified_at: transcript.modified_at,
            bead_id: transcript.bead_id.as_ref().map(|id| id.to_string()),
            bead_type: None, // Will be filled in by caller
            task_keywords,
            tools_used,
            file_patterns,
            action_counts,
        }
    }

    /// Extract normalized keywords from text.
    fn extract_keywords(text: &str) -> HashSet<String> {
        let mut keywords = HashSet::new();

        // Common stop words to filter out
        let stop_words = [
            "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for",
            "of", "with", "by", "from", "as", "is", "was", "are", "were", "been",
            "be", "have", "has", "had", "do", "does", "did", "will", "would", "could",
            "should", "may", "might", "must", "shall", "can", "need", "this", "that",
            "these", "those", "i", "you", "he", "she", "it", "we", "they", "what",
            "which", "who", "when", "where", "why", "how", "all", "each", "every",
            "both", "few", "more", "most", "other", "some", "such", "no", "nor",
            "not", "only", "own", "same", "so", "than", "too", "very", "just",
            "also", "now", "here", "there", "then", "once", "about", "into",
            "through", "during", "before", "after", "above", "below", "up", "down",
        ];

        for word in text.split_whitespace() {
            // Clean the word: remove punctuation, convert to lowercase
            let cleaned = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();

            // Skip short words and stop words
            if cleaned.len() >= 3 && !stop_words.contains(&cleaned.as_str()) {
                keywords.insert(cleaned);
            }
        }

        keywords
    }

    /// Extract file pattern from action description.
    fn extract_file_pattern(desc: &str) -> Option<String> {
        // Look for file paths in descriptions like "Read: src/file.rs"
        if let Some(idx) = desc.find(": ") {
            let after = &desc[idx + 2..];
            let path = after.split_whitespace().next()?;
            // Normalize to a pattern: replace specific names with wildcards
            // e.g., "src/drift/mod.rs" -> "src/drift/*.rs"
            if let Some(ext_idx) = path.rfind('.') {
                let ext = &path[ext_idx..];
                let base = &path[..ext_idx];
                if let Some(last_slash) = base.rfind('/') {
                    let dir = &base[..last_slash];
                    return Some(format!("{}/*{}", dir, ext));
                }
            }
        }
        None
    }

    /// Compute Jaccard similarity with another fingerprint.
    ///
    /// Jaccard similarity = |intersection| / |union|
    /// Returns 0.0 for completely different, 1.0 for identical.
    pub fn jaccard_similarity(&self, other: &SessionFingerprint) -> f64 {
        // Compare multiple aspects and weight them

        // Tool usage similarity (40% weight)
        let tool_sim = if self.tools_used.is_empty() && other.tools_used.is_empty() {
            1.0
        } else {
            let intersection = self.tools_used.intersection(&other.tools_used).count();
            let union = self.tools_used.union(&other.tools_used).count();
            if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
        };

        // Keyword similarity (40% weight)
        let kw_sim = if self.task_keywords.is_empty() && other.task_keywords.is_empty() {
            1.0
        } else {
            let intersection = self.task_keywords.intersection(&other.task_keywords).count();
            let union = self.task_keywords.union(&other.task_keywords).count();
            if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
        };

        // File pattern similarity (20% weight)
        let file_sim = if self.file_patterns.is_empty() && other.file_patterns.is_empty() {
            1.0
        } else {
            let intersection = self.file_patterns.intersection(&other.file_patterns).count();
            let union = self.file_patterns.union(&other.file_patterns).count();
            if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
        };

        tool_sim * 0.4 + kw_sim * 0.4 + file_sim * 0.2
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Drift Category
// ──────────────────────────────────────────────────────────────────────────────

/// Classification of detected drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriftCategory {
    /// Approaches show clear temporal progression (A → B → C).
    Evolved,
    /// Approaches vary randomly without clear pattern.
    Inconsistent,
    /// Not enough data to categorize.
    Unknown,
}

impl DriftCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriftCategory::Evolved => "evolved",
            DriftCategory::Inconsistent => "inconsistent",
            DriftCategory::Unknown => "unknown",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Approach Difference
// ──────────────────────────────────────────────────────────────────────────────

/// A specific difference detected between two sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproachDifference {
    /// Description of what differs.
    pub description: String,
    /// Tool unique to session A (if any).
    pub tool_only_a: Option<String>,
    /// Tool unique to session B (if any).
    pub tool_only_b: Option<String>,
    /// File pattern unique to session A.
    pub file_only_a: Option<String>,
    /// File pattern unique to session B.
    pub file_only_b: Option<String>,
    /// Keyword unique to session A.
    pub keyword_only_a: Option<String>,
    /// Keyword unique to session B.
    pub keyword_only_b: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Drift Cluster
// ──────────────────────────────────────────────────────────────────────────────

/// A cluster of similar sessions with detected drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftCluster {
    /// Unique cluster ID.
    pub cluster_id: String,
    /// Representative bead type for this cluster.
    pub bead_type: Option<BeadType>,
    /// All sessions in this cluster.
    pub sessions: Vec<SessionFingerprint>,
    /// Differences detected between sessions.
    pub differences: Vec<ApproachDifference>,
    /// Drift categorization.
    pub category: DriftCategory,
    /// When this drift was detected.
    pub detected_at: DateTime<Utc>,
}

impl DriftCluster {
    /// Create a new drift cluster.
    pub fn new(cluster_id: String, sessions: Vec<SessionFingerprint>) -> Self {
        let bead_type = sessions.first()
            .and_then(|s| s.bead_type.clone());

        DriftCluster {
            cluster_id,
            bead_type,
            sessions,
            differences: Vec::new(),
            category: DriftCategory::Unknown,
            detected_at: Utc::now(),
        }
    }

    /// Analyze differences between sessions and categorize drift.
    pub fn analyze(&mut self) {
        if self.sessions.len() < 2 {
            return;
        }

        // Sort sessions by time
        let mut sorted = self.sessions.clone();
        sorted.sort_by_key(|s| s.modified_at);

        // Detect differences between consecutive sessions
        for window in sorted.windows(2) {
            let (a, b) = (&window[0], &window[1]);

            // Find tools only in A
            for tool in a.tools_used.difference(&b.tools_used) {
                self.differences.push(ApproachDifference {
                    description: format!("Session A used `{}` tool, session B did not", tool),
                    tool_only_a: Some(tool.clone()),
                    tool_only_b: None,
                    file_only_a: None,
                    file_only_b: None,
                    keyword_only_a: None,
                    keyword_only_b: None,
                });
            }

            // Find tools only in B
            for tool in b.tools_used.difference(&a.tools_used) {
                self.differences.push(ApproachDifference {
                    description: format!("Session B used `{}` tool, session A did not", tool),
                    tool_only_a: None,
                    tool_only_b: Some(tool.clone()),
                    file_only_a: None,
                    file_only_b: None,
                    keyword_only_a: None,
                    keyword_only_b: None,
                });
            }

            // Find file pattern differences
            for file in a.file_patterns.difference(&b.file_patterns) {
                self.differences.push(ApproachDifference {
                    description: format!("Session A touched `{}`, session B did not", file),
                    tool_only_a: None,
                    tool_only_b: None,
                    file_only_a: Some(file.clone()),
                    file_only_b: None,
                    keyword_only_a: None,
                    keyword_only_b: None,
                });
            }

            for file in b.file_patterns.difference(&a.file_patterns) {
                self.differences.push(ApproachDifference {
                    description: format!("Session B touched `{}`, session A did not", file),
                    tool_only_a: None,
                    tool_only_b: None,
                    file_only_a: None,
                    file_only_b: Some(file.clone()),
                    keyword_only_a: None,
                    keyword_only_b: None,
                });
            }
        }

        // Categorize drift based on temporal patterns
        self.category = self.categorize_drift();
    }

    /// Categorize drift as evolved or inconsistent based on temporal patterns.
    fn categorize_drift(&self) -> DriftCategory {
        if self.sessions.len() < 2 {
            return DriftCategory::Unknown;
        }

        let mut sorted = self.sessions.clone();
        sorted.sort_by_key(|s| s.modified_at);

        // Check for clear temporal progression in tool usage
        // Evolved: early sessions use tool A, later sessions consistently use tool B
        // Inconsistent: random alternation

        // Split sessions into early and late halves
        let mid = sorted.len() / 2;
        let early_tools: HashSet<_> = sorted[..mid].iter()
            .flat_map(|s| s.tools_used.iter().cloned())
            .collect();
        let _late_tools: HashSet<_> = sorted[mid..].iter()
            .flat_map(|s| s.tools_used.iter().cloned())
            .collect();

        // Check if late sessions converged on a consistent approach
        let late_unique_tools: Vec<HashSet<_>> = sorted[mid..].iter()
            .map(|s| s.tools_used.clone())
            .collect();

        // If all late sessions use the same tool set, that's evolution
        if late_unique_tools.len() > 1 {
            let first_tools = &late_unique_tools[0];
            let all_same = late_unique_tools.iter().all(|t| t == first_tools);

            if all_same && first_tools != &early_tools {
                return DriftCategory::Evolved;
            }
        }

        // Check for inconsistent tool usage
        let tool_sets: Vec<_> = sorted.iter()
            .map(|s| s.tools_used.clone())
            .collect();

        // If tool sets vary widely, it's inconsistent
        let unique_tool_count = tool_sets.len();
        if unique_tool_count > sorted.len() / 2 {
            return DriftCategory::Inconsistent;
        }

        // Default to unknown if we can't clearly categorize
        DriftCategory::Unknown
    }

    /// Format as markdown for drift report.
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("## Drift Cluster: {}\n\n", self.cluster_id));
        output.push_str(&format!("**Category:** {}\n", self.category.as_str()));
        if let Some(ref bt) = self.bead_type {
            output.push_str(&format!("**Bead Type:** {}\n", bt.as_str()));
        }
        output.push_str(&format!("**Sessions:** {}\n", self.sessions.len()));
        output.push_str(&format!("**Detected:** {}\n\n", self.detected_at.format("%Y-%m-%d %H:%M:%S UTC")));

        output.push_str("### Sessions\n\n");
        for session in &self.sessions {
            output.push_str(&format!("- **{}** ({})\n", session.session_id,
                session.modified_at.format("%Y-%m-%d %H:%M")));
            if let Some(ref bead_id) = session.bead_id {
                output.push_str(&format!("  - Bead: `{}`\n", bead_id));
            }
            if !session.tools_used.is_empty() {
                output.push_str(&format!("  - Tools: {}\n",
                    session.tools_used.iter().cloned().collect::<Vec<_>>().join(", ")));
            }
        }

        if !self.differences.is_empty() {
            output.push_str("\n### Approach Differences\n\n");
            for diff in &self.differences {
                output.push_str(&format!("- {}\n", diff.description));
            }
        }

        output.push('\n');
        output
    }

    /// Convert this drift cluster into learning entries for the consolidation pipeline.
    ///
    /// Returns a vector of learning entries representing the drift patterns detected.
    /// Each entry captures a specific aspect of the drift:
    /// - For "evolved" drift: the progression pattern
    /// - For "inconsistent" drift: the inconsistency pattern
    /// - For specific tool differences: the tool usage divergence
    pub fn to_learning_entries(&self) -> Vec<LearningEntry> {
        let mut entries = Vec::new();

        // Create a drift ID as the source identifier
        let drift_id = format!("drift-{}", self.cluster_id);
        let worker = "needle-drift".to_string();

        // Main drift observation entry
        let main_observation = match self.category {
            DriftCategory::Evolved => {
                format!(
                    "Drift detected: Worker approach evolved across sessions. Sessions handling similar tasks progressed from early to later approaches, indicating learning and improvement over time."
                )
            }
            DriftCategory::Inconsistent => {
                format!(
                    "Drift detected: Inconsistent approaches across similar sessions. Workers used different tools, file patterns, or action sequences for similar task types without clear temporal progression — may indicate need for standardization."
                )
            }
            DriftCategory::Unknown => {
                format!(
                    "Drift detected: Divergent approaches across similar sessions. Sessions with similar task keywords and file patterns showed different tool usage or action sequences."
                )
            }
        };

        let bead_type = self.bead_type.clone().unwrap_or(BeadType::Other);

        // Main drift entry
        entries.push(LearningEntry::new(
            drift_id.clone(),
            worker.clone(),
            bead_type.clone(),
            main_observation,
            Confidence::High, // Drift patterns are high-signal
            format!("drift-cluster: {}", self.cluster_id),
        ));

        // Add entries for specific tool differences
        for diff in &self.differences {
            if diff.tool_only_a.is_some() || diff.tool_only_b.is_some() {
                let tool_observation = format!(
                    "Tool usage drift across sessions: {}",
                    diff.description
                );
                entries.push(LearningEntry::new(
                    format!("{}-tool", drift_id),
                    worker.clone(),
                    bead_type.clone(),
                    tool_observation,
                    Confidence::Medium,
                    format!("drift-cluster: {}", self.cluster_id),
                ));
            }

            // Add entries for file pattern differences
            if diff.file_only_a.is_some() || diff.file_only_b.is_some() {
                let file_observation = format!(
                    "File access pattern drift across sessions: {}",
                    diff.description
                );
                entries.push(LearningEntry::new(
                    format!("{}-file", drift_id),
                    worker.clone(),
                    bead_type.clone(),
                    file_observation,
                    Confidence::Medium,
                    format!("drift-cluster: {}", self.cluster_id),
                ));
            }
        }

        // For evolved drift, add an entry about the progression pattern
        if self.category == DriftCategory::Evolved && self.sessions.len() >= 2 {
            let mut sorted = self.sessions.clone();
            sorted.sort_by_key(|s| s.modified_at);

            let early_tools: HashSet<_> = sorted[..sorted.len()/2].iter()
                .flat_map(|s| s.tools_used.iter().cloned())
                .collect();
            let late_tools: HashSet<_> = sorted[sorted.len()/2..].iter()
                .flat_map(|s| s.tools_used.iter().cloned())
                .collect();

            if !early_tools.is_empty() && !late_tools.is_empty() {
                let progression_observation = format!(
                    "Evolution pattern detected: Early sessions primarily used {:?}, while later sessions converged on {:?}. This represents a learned improvement in approach.",
                    early_tools.iter().cloned().collect::<Vec<_>>(),
                    late_tools.iter().cloned().collect::<Vec<_>>()
                );
                entries.push(LearningEntry::new(
                    format!("{}-evolution", drift_id),
                    worker,
                    bead_type,
                    progression_observation,
                    Confidence::High,
                    format!("drift-cluster: {}", self.cluster_id),
                ));
            }
        }

        entries
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Drift Report
// ──────────────────────────────────────────────────────────────────────────────

/// Complete drift detection report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftReport {
    /// When the report was generated.
    pub generated_at: DateTime<Utc>,
    /// Number of sessions analyzed.
    pub sessions_analyzed: usize,
    /// Number of clusters with drift detected.
    pub clusters_detected: usize,
    /// All drift clusters found.
    pub clusters: Vec<DriftCluster>,
}

impl DriftReport {
    /// Create a new drift report.
    pub fn new() -> Self {
        DriftReport {
            generated_at: Utc::now(),
            sessions_analyzed: 0,
            clusters_detected: 0,
            clusters: Vec::new(),
        }
    }

    /// Add a cluster to the report.
    pub fn add_cluster(&mut self, cluster: DriftCluster) {
        self.clusters.push(cluster);
        self.clusters_detected = self.clusters.len();
    }

    /// Format as markdown for writing to file.
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str("# Drift Detection Report\n\n");
        output.push_str(&format!("**Generated:** {}\n", self.generated_at.format("%Y-%m-%d %H:%M:%S UTC")));
        output.push_str(&format!("**Sessions Analyzed:** {}\n", self.sessions_analyzed));
        output.push_str(&format!("**Drift Clusters:** {}\n\n", self.clusters_detected));

        if self.clusters.is_empty() {
            output.push_str("No drift detected across analyzed sessions.\n");
        } else {
            for cluster in &self.clusters {
                output.push_str(&cluster.to_markdown());
            }
        }

        output
    }

    /// Write report to file.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", path.display()))?;
        }

        std::fs::write(path, self.to_markdown())
            .with_context(|| format!("failed to write drift report: {}", path.display()))?;

        Ok(())
    }

    /// Extract all learning entries from drift clusters in this report.
    ///
    /// This enables feeding drift detection results back into the learning
    /// consolidation pipeline as high-signal inputs.
    pub fn to_learning_entries(&self) -> Vec<LearningEntry> {
        let mut entries = Vec::new();

        for cluster in &self.clusters {
            entries.extend(cluster.to_learning_entries());
        }

        entries
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Drift Detector
// ──────────────────────────────────────────────────────────────────────────────

/// Detects drift across multiple sessions.
pub struct DriftDetector {
    /// Jaccard similarity threshold for clustering sessions (default: 0.6).
    similarity_threshold: f64,
}

impl DriftDetector {
    /// Create a new drift detector.
    pub fn new(similarity_threshold: f64) -> Self {
        DriftDetector {
            similarity_threshold,
        }
    }

    /// Detect drift across all provided transcripts.
    pub fn detect(&self, transcripts: &[ParsedTranscript]) -> Result<DriftReport> {
        let mut report = DriftReport::new();

        if transcripts.len() < 2 {
            return Ok(report);
        }

        // Build fingerprints
        let mut fingerprints: Vec<SessionFingerprint> = transcripts
            .iter()
            .map(SessionFingerprint::from_transcript)
            .collect();

        // Sort by modification time (for temporal analysis)
        fingerprints.sort_by_key(|f| f.modified_at);

        // Track total sessions that were analyzed for drift
        let total_sessions_analyzed = fingerprints.len();

        // Cluster similar sessions using greedy clustering
        let mut clusters: Vec<DriftCluster> = Vec::new();
        let mut assigned = vec![false; fingerprints.len()];

        for (i, fp) in fingerprints.iter().enumerate() {
            if assigned[i] {
                continue;
            }

            let mut cluster_sessions = vec![fp.clone()];
            assigned[i] = true;

            // Find all similar sessions
            for (j, other) in fingerprints.iter().enumerate() {
                if i == j || assigned[j] {
                    continue;
                }

                let sim = fp.jaccard_similarity(other);
                if sim >= self.similarity_threshold {
                    cluster_sessions.push(other.clone());
                    assigned[j] = true;
                }
            }

            // Only create a cluster if we have 2+ sessions
            if cluster_sessions.len() >= 2 {
                let cluster_id = format!("drift-{}", clusters.len());
                let mut cluster = DriftCluster::new(cluster_id, cluster_sessions);
                cluster.analyze();

                // Only keep clusters with actual differences
                if !cluster.differences.is_empty() {
                    clusters.push(cluster);
                }
            }
        }

        // Set total sessions analyzed (all transcripts processed)
        report.sessions_analyzed = total_sessions_analyzed;

        // Add all clusters to report
        for cluster in clusters {
            report.add_cluster(cluster);
        }

        Ok(report)
    }
}

impl Default for DriftDetector {
    fn default() -> Self {
        DriftDetector::new(0.6)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{TranscriptAction, ActionType};

    fn make_test_transcript(
        session_id: &str,
        task: &str,
        tools: &[&str],
    ) -> ParsedTranscript {
        let actions = tools.iter().map(|tool| TranscriptAction {
            action_type: ActionType::ToolUse,
            tool_name: Some(tool.to_string()),
            description: format!("{}: /tmp/test.txt", tool),
        }).collect();

        ParsedTranscript {
            session_id: session_id.to_string(),
            modified_at: Utc::now(),
            task_description: Some(task.to_string()),
            actions,
            bead_id: None,
        }
    }

    #[test]
    fn fingerprint_from_transcript() {
        let transcript = make_test_transcript(
            "test-session",
            "Fix the parsing bug in the drift module",
            &["Read", "Edit"],
        );

        let fp = SessionFingerprint::from_transcript(&transcript);

        assert_eq!(fp.session_id, "test-session");
        assert!(fp.tools_used.contains("Read"));
        assert!(fp.tools_used.contains("Edit"));
        assert!(!fp.task_keywords.is_empty());
    }

    #[test]
    fn jaccard_similarity_identical() {
        let transcript = make_test_transcript("id", "test task", &["Read"]);
        let fp1 = SessionFingerprint::from_transcript(&transcript);
        let fp2 = SessionFingerprint::from_transcript(&transcript);

        assert!((fp1.jaccard_similarity(&fp2) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_similarity_different() {
        let t1 = make_test_transcript("id1", "write rust code", &["Write"]);
        let t2 = make_test_transcript("id2", "parse python script", &["Bash"]);

        let fp1 = SessionFingerprint::from_transcript(&t1);
        let fp2 = SessionFingerprint::from_transcript(&t2);

        assert!(fp1.jaccard_similarity(&fp2) < 0.5);
    }

    #[test]
    fn drift_detector_no_transcripts() {
        let detector = DriftDetector::new(0.6);
        let report = detector.detect(&[]).unwrap();

        assert_eq!(report.sessions_analyzed, 0);
        assert_eq!(report.clusters_detected, 0);
    }

    #[test]
    fn drift_detector_single_transcript() {
        let detector = DriftDetector::new(0.6);
        let transcript = make_test_transcript("id", "task", &["Read"]);
        let report = detector.detect(&[transcript]).unwrap();

        assert_eq!(report.sessions_analyzed, 0); // No clusters with <2 sessions
        assert_eq!(report.clusters_detected, 0);
    }

    #[test]
    fn drift_detector_similar_sessions() {
        let detector = DriftDetector::new(0.3); // Low threshold
        let t1 = make_test_transcript("id1", "fix bug", &["Read", "Edit"]);
        let t2 = make_test_transcript("id2", "fix bug", &["Read", "Edit"]);

        let report = detector.detect(&[t1, t2]).unwrap();

        // Similar sessions should form a cluster, but since they're identical,
        // there won't be differences detected
        assert_eq!(report.sessions_analyzed, 2);
        assert_eq!(report.clusters_detected, 0); // No differences = no drift
    }

    #[test]
    fn drift_detector_different_tools() {
        let detector = DriftDetector::new(0.5);
        let t1 = make_test_transcript("id1", "fix parsing", &["Read", "Edit"]);
        let t2 = make_test_transcript("id2", "fix parsing", &["Bash", "Write"]);

        let report = detector.detect(&[t1, t2]).unwrap();

        // Different tools for similar task should create drift
        assert_eq!(report.sessions_analyzed, 2);
        // May or may not create cluster depending on similarity threshold
    }

    #[test]
    fn drift_report_to_markdown() {
        let mut report = DriftReport::new();
        report.sessions_analyzed = 5;
        report.clusters_detected = 1;

        let markdown = report.to_markdown();

        assert!(markdown.contains("# Drift Detection Report"));
        assert!(markdown.contains("**Sessions Analyzed:** 5"));
        assert!(markdown.contains("**Drift Clusters:** 1"));
    }

    #[test]
    fn drift_category_as_str() {
        assert_eq!(DriftCategory::Evolved.as_str(), "evolved");
        assert_eq!(DriftCategory::Inconsistent.as_str(), "inconsistent");
        assert_eq!(DriftCategory::Unknown.as_str(), "unknown");
    }
}
