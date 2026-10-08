//! Append-once execution checkpoints in the existing attempt trace directory.
//!
//! Lifecycle producers are separate work. This adapter has no background
//! activation: a caller must present a current, fenced claim for every append.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use needle_learning::{validate_checkpoint, ExecutionCheckpoint};

use crate::bead_store::BeadStore;
use crate::claim::ClaimIdentity;
use crate::sanitize::Sanitizer;
use crate::trace::attempt_scoped_dir;
use crate::types::{BeadId, ClaimStatus};

/// A successful append either published one new immutable record or did no IO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Written,
    AlreadyPresent,
}

/// A local view of the attempt's checkpoint files. The workspace must be the
/// authoritative target workspace for the supplied bead store.
pub struct ExecutionCheckpointStore<'a> {
    workspace: &'a Path,
    bead_id: &'a BeadId,
    attempt_id: &'a str,
}

impl<'a> ExecutionCheckpointStore<'a> {
    pub fn new(workspace: &'a Path, bead_id: &'a BeadId, attempt_id: &'a str) -> Result<Self> {
        if !safe_component(bead_id.as_ref()) || !safe_component(attempt_id) {
            bail!("invalid checkpoint bead or attempt path component");
        }
        Ok(Self {
            workspace,
            bead_id,
            attempt_id,
        })
    }

    /// Verify live ownership, reject unsanitized fields, then publish one
    /// immutable checkpoint. A backend read failure fails closed.
    ///
    /// The live claim read occurs immediately before local publication. This
    /// does not turn the filesystem and bead store into one transaction; a
    /// later resolution must still use the backend's fenced claim operation.
    pub async fn append(
        &self,
        record: &ExecutionCheckpoint,
        held: &ClaimIdentity,
        store: &dyn BeadStore,
        sanitizer: &Sanitizer,
    ) -> Result<AppendOutcome> {
        self.validate_record(record, sanitizer)?;
        let live = store
            .claim_status(self.bead_id)
            .await
            .with_context(|| format!("checkpoint ownership read failed for {}", self.bead_id))?;
        self.append_with_status(record, held, &live, sanitizer)
    }

    fn validate_record(&self, record: &ExecutionCheckpoint, sanitizer: &Sanitizer) -> Result<()> {
        validate_checkpoint(record, |text| sanitizer.sanitize(text) != text)?;
        if record.bead_id.as_str() != self.bead_id.as_ref()
            || record.attempt_id.as_str() != self.attempt_id
        {
            bail!("checkpoint identity does not match the target bead and attempt");
        }
        Ok(())
    }

    fn append_with_status(
        &self,
        record: &ExecutionCheckpoint,
        held: &ClaimIdentity,
        live: &ClaimStatus,
        sanitizer: &Sanitizer,
    ) -> Result<AppendOutcome> {
        self.validate_record(record, sanitizer)?;
        if held.claim_epoch != Some(record.claim_epoch.0) {
            bail!("checkpoint claim epoch differs from held claim");
        }
        let mismatches = held.mismatches(live);
        if !mismatches.is_empty() {
            bail!("stale checkpoint ownership: {}", mismatches.join(", "));
        }
        if let Some(ownership) = &record.ownership {
            if ownership.actor != held.actor || ownership.revision != held.revision {
                bail!("checkpoint ownership does not match the held claim identity");
            }
        }
        if live.claim_epoch != Some(record.claim_epoch.0) {
            bail!("stale checkpoint claim epoch");
        }

        let dir = self.attempt_dir();
        fs::create_dir_all(&dir).with_context(|| {
            format!(
                "failed to create checkpoint trace directory {}",
                dir.display()
            )
        })?;
        self.publish(record, &dir, sanitizer)
    }

    fn publish(
        &self,
        record: &ExecutionCheckpoint,
        dir: &Path,
        sanitizer: &Sanitizer,
    ) -> Result<AppendOutcome> {
        let path = checkpoint_path(dir, record);
        if path.exists() {
            let previous = read_record(&path, sanitizer)?;
            if previous == *record {
                return Ok(AppendOutcome::AlreadyPresent);
            }
            bail!(
                "conflicting checkpoint payload for {}",
                record.checkpoint_id
            );
        }

        let encoded =
            serde_json::to_vec(record).context("failed to encode execution checkpoint")?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir).with_context(|| {
            format!(
                "failed to create checkpoint temporary file in {}",
                dir.display()
            )
        })?;
        use std::io::Write;
        temporary
            .write_all(&encoded)
            .context("failed to write checkpoint temporary file")?;
        temporary
            .as_file()
            .sync_all()
            .context("failed to sync checkpoint temporary file")?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let previous = read_record(&path, sanitizer)?;
                if previous == *record {
                    return Ok(AppendOutcome::AlreadyPresent);
                }
                bail!(
                    "conflicting checkpoint payload for {}",
                    record.checkpoint_id
                );
            }
            Err(error) => {
                return Err(error.error)
                    .with_context(|| format!("failed to publish checkpoint {}", path.display()))
            }
        }
        File::open(dir)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("failed to sync checkpoint directory {}", dir.display()))?;
        Ok(AppendOutcome::Written)
    }

    /// Read all accepted records for this attempt. Incomplete temporary files
    /// from interrupted writes are ignored; a malformed published file fails.
    pub fn read_all(&self, sanitizer: &Sanitizer) -> Result<Vec<ExecutionCheckpoint>> {
        let dir = self.attempt_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&dir)
            .with_context(|| format!("failed to read checkpoint directory {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("failed to enumerate checkpoints in {}", dir.display()))?;
        let mut paths = entries
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("execution-checkpoint-") && name.ends_with(".json")
                    })
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let record = read_record(&path, sanitizer)?;
                if record.bead_id.as_str() != self.bead_id.as_ref()
                    || record.attempt_id.as_str() != self.attempt_id
                    || checkpoint_path(&dir, &record) != path
                {
                    bail!("checkpoint identity mismatch in {}", path.display());
                }
                Ok(record)
            })
            .collect()
    }

    fn attempt_dir(&self) -> PathBuf {
        let bead_dir = self
            .workspace
            .join(".beads")
            .join("traces")
            .join(self.bead_id.as_ref());
        attempt_scoped_dir(&bead_dir, Some(self.attempt_id))
    }
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn checkpoint_path(dir: &Path, record: &ExecutionCheckpoint) -> PathBuf {
    dir.join(format!(
        "execution-checkpoint-{}-{}.json",
        record.claim_epoch.0, record.checkpoint_id
    ))
}

fn read_record(path: &Path, sanitizer: &Sanitizer) -> Result<ExecutionCheckpoint> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect checkpoint {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.len() > needle_learning::execution_checkpoint::MAX_CHECKPOINT_BYTES as u64
    {
        bail!("invalid checkpoint file {}", path.display());
    }
    let bytes =
        fs::read(path).with_context(|| format!("failed to read checkpoint {}", path.display()))?;
    let record: ExecutionCheckpoint = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed checkpoint {}", path.display()))?;
    validate_checkpoint(&record, |text| sanitizer.sanitize(text) != text)
        .with_context(|| format!("invalid checkpoint {}", path.display()))?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize::CustomPattern;
    use crate::types::BeadStatus;
    use needle_learning::{
        AttemptId, CheckpointEvidence, CheckpointOwnership, ContentHash, EvidenceId, EvidenceRef,
        FencingEpoch, RecoveryAttribution, RecoveryDecision, SourceId, Timestamp,
        CURRENT_SCHEMA_VERSION,
    };

    fn sanitizer() -> Sanitizer {
        Sanitizer::new(&[CustomPattern {
            id: "fixture-secret".into(),
            pattern: "secret-[A-Za-z0-9]+".into(),
            entropy: None,
        }])
        .unwrap()
    }

    fn record() -> ExecutionCheckpoint {
        ExecutionCheckpoint {
            schema_version: CURRENT_SCHEMA_VERSION,
            checkpoint_id: "first-gate".into(),
            bead_id: needle_learning::BeadId::new("needle-test").unwrap(),
            attempt_id: AttemptId::new("attempt-1").unwrap(),
            claim_epoch: FencingEpoch(4),
            ownership: None,
            timestamp: Timestamp::new("2026-10-07T12:00:00Z").unwrap(),
            intended_result: "Gate passes".into(),
            observable_result: "Gate failed on case 2".into(),
            rationale: Some("One assertion needs repair".into()),
            next_intervention: "Change the assertion".into(),
            recovery_decision: None,
            evidence_refs: vec![CheckpointEvidence::UncommittedRecovery {
                reference: EvidenceRef {
                    evidence_id: EvidenceId::new("diff-1").unwrap(),
                    digest: ContentHash::new("sha256:diff").unwrap(),
                    source: SourceId::new("trace/recovery.diff").unwrap(),
                },
                attribution: RecoveryAttribution::Ambiguous,
            }],
        }
    }

    fn claim(epoch: u64) -> (ClaimIdentity, ClaimStatus) {
        (
            ClaimIdentity {
                actor: "worker-1".into(),
                revision: Some(12),
                claim_epoch: Some(epoch),
            },
            ClaimStatus {
                status: BeadStatus::InProgress,
                assignee: Some("worker-1".into()),
                revision: Some(12),
                claim_epoch: Some(epoch),
            },
        )
    }

    #[test]
    fn append_replay_conflict_and_restart_preserve_accepted_record() {
        let workspace = tempfile::tempdir().unwrap();
        let bead_id = BeadId::from("needle-test");
        let checkpoints =
            ExecutionCheckpointStore::new(workspace.path(), &bead_id, "attempt-1").unwrap();
        let value = record();
        let (held, live) = claim(4);
        let sanitizer = sanitizer();
        assert_eq!(
            checkpoints
                .append_with_status(&value, &held, &live, &sanitizer)
                .unwrap(),
            AppendOutcome::Written
        );
        let path = checkpoint_path(&checkpoints.attempt_dir(), &value);
        let first_bytes = fs::read(&path).unwrap();
        let first_modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            checkpoints
                .append_with_status(&value, &held, &live, &sanitizer)
                .unwrap(),
            AppendOutcome::AlreadyPresent
        );
        assert_eq!(fs::read(&path).unwrap(), first_bytes);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            first_modified
        );
        let mut conflict = value.clone();
        conflict.observable_result = "Different result".into();
        assert!(checkpoints
            .append_with_status(&conflict, &held, &live, &sanitizer)
            .unwrap_err()
            .to_string()
            .contains("conflicting checkpoint payload"));

        // A crashed writer leaves only its private temporary file. A new
        // process can still read the accepted record and publish another one.
        fs::write(
            checkpoints.attempt_dir().join(".tmp-torn-checkpoint"),
            b"{\"schema_version\":1",
        )
        .unwrap();
        let restarted =
            ExecutionCheckpointStore::new(workspace.path(), &bead_id, "attempt-1").unwrap();
        assert_eq!(restarted.read_all(&sanitizer).unwrap(), vec![value.clone()]);
        let mut next = value.clone();
        next.checkpoint_id = "second-gate".into();
        assert_eq!(
            restarted
                .append_with_status(&next, &held, &live, &sanitizer)
                .unwrap(),
            AppendOutcome::Written
        );
        assert_eq!(restarted.read_all(&sanitizer).unwrap().len(), 2);
        assert_eq!(fs::read(&path).unwrap(), first_bytes);
        fs::write(
            restarted
                .attempt_dir()
                .join("execution-checkpoint-4-broken.json"),
            b"{\"schema_version\":1",
        )
        .unwrap();
        assert!(restarted
            .read_all(&sanitizer)
            .unwrap_err()
            .to_string()
            .contains("malformed checkpoint"));
    }

    #[test]
    fn stale_owner_mismatch_and_sensitive_evidence_fail_without_publishing() {
        let workspace = tempfile::tempdir().unwrap();
        let bead_id = BeadId::from("needle-test");
        let checkpoints =
            ExecutionCheckpointStore::new(workspace.path(), &bead_id, "attempt-1").unwrap();

        let mut wrong_owner = record();
        wrong_owner.schema_version = needle_learning::CURRENT_EXECUTION_CHECKPOINT_VERSION;
        wrong_owner.recovery_decision = Some(RecoveryDecision::Retry);
        wrong_owner.ownership = Some(CheckpointOwnership {
            actor: "worker-elsewhere".into(),
            revision: Some(12),
        });
        wrong_owner.evidence_refs = vec![CheckpointEvidence::RecoveryDecision {
            reference: EvidenceRef {
                evidence_id: EvidenceId::new("recovery-decision").unwrap(),
                digest: ContentHash::new("sha256:decision").unwrap(),
                source: SourceId::new("attempt-resolved-attempt-1").unwrap(),
            },
        }];
        let (held, live) = claim(4);
        assert!(checkpoints
            .append_with_status(&wrong_owner, &held, &live, &sanitizer())
            .unwrap_err()
            .to_string()
            .contains("does not match the held claim identity"));
        assert!(!checkpoints.attempt_dir().exists());

        let value = record();
        let (held, mut live) = claim(4);
        live.claim_epoch = Some(5);
        assert!(checkpoints
            .append_with_status(&value, &held, &live, &sanitizer())
            .unwrap_err()
            .to_string()
            .contains("stale checkpoint ownership"));
        assert!(!checkpoints.attempt_dir().exists());

        let (_, live) = claim(4);
        let mut sensitive = value.clone();
        sensitive.evidence_refs[0] = CheckpointEvidence::VerifiedArtifact {
            reference: EvidenceRef {
                evidence_id: EvidenceId::new("artifact-1").unwrap(),
                digest: ContentHash::new("sha256:artifact").unwrap(),
                source: SourceId::new("trace/secret-ABC123.txt").unwrap(),
            },
            artifact_digest: ContentHash::new("sha256:commit").unwrap(),
        };
        assert!(checkpoints
            .append_with_status(&sensitive, &held, &live, &sanitizer())
            .unwrap_err()
            .to_string()
            .contains("sensitive checkpoint field evidence_source"));
        assert!(!checkpoints.attempt_dir().exists());
        assert!(ExecutionCheckpointStore::new(workspace.path(), &bead_id, "../other").is_err());
    }
}
