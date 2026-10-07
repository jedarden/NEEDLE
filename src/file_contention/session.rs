//! Per-dispatch file-contention lifecycle (needle-04798783).
//!
//! The worker never intercepts writes: a capable harness's pre-write hook
//! calls `needle contention acquire` lazily on its first create/edit/delete/
//! rename intent and renews on later writes. The worker's part is the
//! bracket around the attempt:
//!
//! - [`prepare`] resolves coverage for this adapter and bead workspace. When
//!   coverage is `enabled`, it makes markers git-safe (local exclude) and
//!   returns the hook-contract environment to add to the agent's env. In any
//!   other state it returns no environment at all, so dispatch stays
//!   byte-for-byte what it was before Phase 20 and no marker tree is created.
//! - [`ContentionSession::release`] removes every marker the attempt owns on
//!   any terminal outcome, and reports marker paths that reached a commit.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::{
    file_contention_for_workspace, FileContentionCoverage, FILE_CONTENTION_HOOK_CAPABILITY,
};
use crate::file_contention::env;
use crate::file_contention::git_safety;
use crate::file_contention::store::{MarkerStore, Participant};

/// Identity the dispatching worker hands the attempt.
#[derive(Debug, Clone)]
pub struct DispatchIdentity<'a> {
    pub bead_id: &'a str,
    pub attempt_id: &'a str,
    pub worker_id: &'a str,
    pub host: &'a str,
    /// Process that stays alive for the whole attempt: the worker itself.
    /// Hooks run through short-lived shells, so their parent pid would read
    /// as dead almost immediately.
    pub holder_pid: u32,
}

/// Outcome of [`prepare`].
#[derive(Debug)]
pub enum Preparation {
    /// No markers for this dispatch. `coverage` is `supported_disabled`,
    /// `unsupported`, or `degraded` (opted in, but markers could not be made
    /// safe — reported, never silently treated as protected).
    Inactive {
        coverage: &'static str,
        reason: Option<String>,
    },
    /// Markers are live for this dispatch.
    Active {
        session: ContentionSession,
        env: Vec<(String, String)>,
    },
}

impl Preparation {
    pub fn coverage(&self) -> &'static str {
        match self {
            Preparation::Inactive { coverage, .. } => coverage,
            Preparation::Active { .. } => FileContentionCoverage::Enabled.as_str(),
        }
    }
}

/// Markers owned by one attempt, released when the attempt ends.
#[derive(Debug, Clone)]
pub struct ContentionSession {
    repo: PathBuf,
    participant: Participant,
}

/// What [`ContentionSession::release`] did.
#[derive(Debug, Default)]
pub struct Release {
    pub released: Vec<String>,
    /// Marker paths present in commits between the pre-dispatch HEAD and the
    /// current HEAD. Should always be empty: git safety excludes and guards
    /// them, so a non-empty list is a defect to surface.
    pub committed_markers: Vec<String>,
}

/// Resolve coverage for one dispatch and, when enabled, the hook-contract
/// environment.
pub fn prepare(
    capabilities: &[String],
    workspace: &Path,
    who: &DispatchIdentity<'_>,
) -> Preparation {
    let config = match file_contention_for_workspace(workspace) {
        Ok(config) => config,
        Err(err) => {
            // An unsupported adapter is unaffected by a bad opt-in block.
            if FileContentionCoverage::resolve(capabilities, &Default::default())
                == FileContentionCoverage::Unsupported
            {
                return Preparation::Inactive {
                    coverage: FileContentionCoverage::Unsupported.as_str(),
                    reason: None,
                };
            }
            return Preparation::Inactive {
                coverage: "degraded",
                reason: Some(format!("invalid file_contention config: {err:#}")),
            };
        }
    };
    let coverage = FileContentionCoverage::resolve(capabilities, &config);
    if !coverage.writes_markers() {
        return Preparation::Inactive {
            coverage: coverage.as_str(),
            reason: None,
        };
    }
    let degraded = |reason: String| Preparation::Inactive {
        coverage: "degraded",
        reason: Some(reason),
    };
    let store = match MarkerStore::open(workspace) {
        Ok(store) => store,
        Err(err) => return degraded(format!("marker store unavailable: {err:#}")),
    };
    if let Err(err) = git_safety::ensure_local_exclude(store.repo_root()) {
        return degraded(format!("cannot exclude markers from git: {err:#}"));
    }
    let participant = Participant {
        bead_id: Some(who.bead_id.to_string()),
        attempt_id: Some(who.attempt_id.to_string()),
        session_id: None,
        worker_id: who.worker_id.to_string(),
        host: who.host.to_string(),
        pid: who.holder_pid,
    };
    if let Err(err) = participant.validate() {
        return degraded(format!("{err:#}"));
    }
    let env = vec![
        (env::COVERAGE.to_string(), coverage.as_str().to_string()),
        (
            env::CONTRACT.to_string(),
            FILE_CONTENTION_HOOK_CAPABILITY.to_string(),
        ),
        (
            env::REPO.to_string(),
            store.repo_root().display().to_string(),
        ),
        (env::LEASE_SECS.to_string(), config.lease_secs.to_string()),
        (env::BEAD_ID.to_string(), who.bead_id.to_string()),
        (env::ATTEMPT_ID.to_string(), who.attempt_id.to_string()),
        (env::WORKER_ID.to_string(), who.worker_id.to_string()),
        (env::HOLDER_PID.to_string(), who.holder_pid.to_string()),
    ];
    Preparation::Active {
        session: ContentionSession {
            repo: store.repo_root().to_path_buf(),
            participant,
        },
        env,
    }
}

impl ContentionSession {
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn participant(&self) -> &Participant {
        &self.participant
    }

    /// Remove every marker this attempt owns, then check whether any marker
    /// reached a commit made since `pre_dispatch_head`.
    pub fn release(&self, pre_dispatch_head: Option<&str>) -> Result<Release> {
        let store = MarkerStore::open(&self.repo)?;
        let released = store.release(&self.participant, None)?;
        let committed_markers = match pre_dispatch_head {
            Some(base) => {
                git_safety::markers_in_range(&self.repo, base, "HEAD").unwrap_or_default()
            }
            None => Vec::new(),
        };
        Ok(Release {
            released,
            committed_markers,
        })
    }
}
