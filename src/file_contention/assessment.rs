//! Classify an existing file-contention marker (plan Phase 20.4,
//! needle-3f3c6825).
//!
//! The next participant combines the marker with the holder's liveness, the
//! owning bead's state when known, and the file's current state against the
//! recorded baseline. The result is ADVICE: it cannot release a bead or confer
//! attempt authority. Only a stale-unchanged marker (or one left by a verified
//! closed bead with an unchanged file) may be reclaimed, atomically, through
//! [`reclaim_or_acquire`]. Stale-modified and ambiguous markers must be
//! preserved and routed by the caller.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::store::{
    AcquireOutcome, Baseline, MarkerRead, MarkerRecord, MarkerStore, Participant, PathIntent,
};

/// Whether the marker's holder is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Fresh heartbeat, or the holder process is running on this host.
    Live,
    /// Provably gone: on this host, no live process and no fresh heartbeat.
    Dead,
    /// Cannot tell (e.g. a holder on another host).
    Unknown,
}

/// What the authoritative bead store says about the holder's bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadState {
    Closed,
    /// The bead's current attempt, when the store exposes one.
    InProgress {
        attempt_id: Option<String>,
    },
    Open,
}

/// External facts the assessment needs. Implemented by the caller (worker,
/// CLI, doctor); tests use a fixture.
pub trait HolderStatus {
    fn liveness(&self, holder: &MarkerRecord) -> Liveness;
    /// `None` when the bead is unknown or the store was not consulted.
    fn bead_state(&self, bead_id: &str) -> Option<BeadState>;
}

/// How the file differs from the marker's baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    /// Same existence and content identity as the baseline. `mtime_changed`
    /// records a timestamp-only difference, which is NOT a modification.
    Unchanged {
        mtime_changed: bool,
    },
    Created,
    Deleted,
    ContentChanged,
}

impl FileState {
    pub fn is_modified(self) -> bool {
        !matches!(self, Self::Unchanged { .. })
    }
}

/// Advice for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    /// No marker: the path is available.
    Free,
    /// The caller's own attempt/session holds it: renew and continue.
    SameAttempt { holder: Box<MarkerRecord> },
    /// Another participant is current. A collision is anticipated: do not
    /// write. `lease_overdue` means a live holder past its nominal lease,
    /// which is a warning, not loss of the holder's position.
    ActiveOther {
        holder: Box<MarkerRecord>,
        lease_overdue: bool,
    },
    /// Holder gone, file matches the baseline: no observed mutation. The
    /// marker may be reclaimed atomically.
    StaleUnchanged {
        holder: Box<MarkerRecord>,
        file: FileState,
    },
    /// Holder gone, file differs from the baseline: possible abandoned work.
    /// Preserve the file; route to the owning bead for resumption.
    StaleModified {
        holder: Box<MarkerRecord>,
        file: FileState,
    },
    /// The holder's bead is verified closed. Reclaimable only if the file is
    /// unchanged; a modified file needs a follow-up inspection.
    VerifiedClosed {
        holder: Box<MarkerRecord>,
        file: FileState,
    },
    /// Corrupt, contradictory, or unverifiable. Never treated as safe.
    Ambiguous {
        reason: String,
        holder: Option<Box<MarkerRecord>>,
    },
}

impl Assessment {
    /// Stable machine-readable name.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::SameAttempt { .. } => "same_attempt",
            Self::ActiveOther { .. } => "active_other",
            Self::StaleUnchanged { .. } => "stale_unchanged",
            Self::StaleModified { .. } => "stale_modified",
            Self::VerifiedClosed { .. } => "verified_closed",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }

    /// The caller may write (possibly after an atomic reclaim).
    pub fn permits_write(&self) -> bool {
        match self {
            Self::Free | Self::SameAttempt { .. } | Self::StaleUnchanged { .. } => true,
            Self::VerifiedClosed { file, .. } => !file.is_modified(),
            Self::ActiveOther { .. } | Self::StaleModified { .. } | Self::Ambiguous { .. } => false,
        }
    }

    /// A marker that may be replaced by another participant.
    pub fn reclaimable(&self) -> bool {
        match self {
            Self::StaleUnchanged { .. } => true,
            Self::VerifiedClosed { file, .. } => !file.is_modified(),
            Self::Free
            | Self::SameAttempt { .. }
            | Self::ActiveOther { .. }
            | Self::StaleModified { .. }
            | Self::Ambiguous { .. } => false,
        }
    }

    pub fn holder(&self) -> Option<&MarkerRecord> {
        match self {
            Self::Free => None,
            Self::SameAttempt { holder }
            | Self::ActiveOther { holder, .. }
            | Self::StaleUnchanged { holder, .. }
            | Self::StaleModified { holder, .. }
            | Self::VerifiedClosed { holder, .. } => Some(holder),
            Self::Ambiguous { holder, .. } => holder.as_deref(),
        }
    }
}

/// Compare the current file with a recorded baseline. Timestamp is a fast
/// signal only: content identity (size + SHA-256) decides modification.
pub fn compare_with_baseline(
    store: &MarkerStore,
    rel: &str,
    baseline: &Baseline,
) -> Result<FileState> {
    let current = store.baseline(rel)?;
    Ok(match (baseline.existed, current.existed) {
        (false, false) => FileState::Unchanged {
            mtime_changed: false,
        },
        (false, true) => FileState::Created,
        (true, false) => FileState::Deleted,
        (true, true) => {
            let same_content = baseline.size == current.size
                && baseline.content_sha256.is_some()
                && baseline.content_sha256 == current.content_sha256;
            if same_content {
                FileState::Unchanged {
                    mtime_changed: baseline.mtime_ns != current.mtime_ns,
                }
            } else {
                FileState::ContentChanged
            }
        }
    })
}

/// Classify the marker for `rel` from `caller`'s point of view.
pub fn assess(
    store: &MarkerStore,
    rel: &str,
    caller: Option<&Participant>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<Assessment> {
    let read = store.read(rel)?;
    classify(store, rel, read.as_ref(), caller, status, now)
}

/// Classify an already-read marker. Used inside the registry critical
/// section by [`reclaim_or_acquire`].
pub fn classify(
    store: &MarkerStore,
    rel: &str,
    read: Option<&MarkerRead>,
    caller: Option<&Participant>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<Assessment> {
    let holder = match read {
        None => return Ok(Assessment::Free),
        Some(MarkerRead::Corrupt { reason }) => {
            return Ok(Assessment::Ambiguous {
                reason: reason.clone(),
                holder: None,
            })
        }
        Some(MarkerRead::Valid(record)) => record.clone(),
    };

    if caller.is_some_and(|c| c.owns(&holder)) {
        return Ok(Assessment::SameAttempt { holder });
    }

    let file = match compare_with_baseline(store, rel, &holder.baseline) {
        Ok(file) => file,
        Err(err) => {
            return Ok(Assessment::Ambiguous {
                reason: format!("cannot compare file with baseline: {err:#}"),
                holder: Some(holder),
            })
        }
    };

    let bead_state = holder
        .bead_id
        .as_deref()
        .and_then(|id| status.bead_state(id));
    if bead_state == Some(BeadState::Closed) {
        return Ok(Assessment::VerifiedClosed { holder, file });
    }

    let liveness = status.liveness(&holder);
    let expired = holder.lease_expired(now);
    // The bead store naming a DIFFERENT current attempt means this marker's
    // attempt is no longer current, whatever its lease says.
    let superseded_attempt = matches!(
        &bead_state,
        Some(BeadState::InProgress { attempt_id: Some(current) })
            if holder.attempt_id.as_deref().is_some_and(|mine| mine != current)
    );
    // The holder's own process/heartbeat contradicts a store that says its
    // attempt was superseded: report rather than guess.
    if superseded_attempt && liveness == Liveness::Live {
        return Ok(Assessment::Ambiguous {
            reason: "holder is live but the bead store names a different current attempt".into(),
            holder: Some(holder),
        });
    }

    let current = match liveness {
        Liveness::Live => true,
        Liveness::Dead => false,
        Liveness::Unknown => !expired && !superseded_attempt,
    };
    if current {
        return Ok(Assessment::ActiveOther {
            lease_overdue: expired,
            holder,
        });
    }
    if file.is_modified() {
        Ok(Assessment::StaleModified { holder, file })
    } else {
        Ok(Assessment::StaleUnchanged { holder, file })
    }
}

/// Acquire `paths` for `caller`, atomically reclaiming any marker whose
/// assessment (made INSIDE the registry critical section) is reclaimable.
/// Active, stale-modified, and ambiguous markers are returned as conflicts and
/// nothing is written.
pub fn reclaim_or_acquire(
    store: &MarkerStore,
    caller: &Participant,
    paths: &[PathIntent],
    lease: Duration,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<AcquireOutcome> {
    let takeover = |rel: &str, read: &MarkerRead| -> bool {
        classify(store, rel, Some(read), Some(caller), status, now)
            .map(|assessment| assessment.reclaimable())
            .unwrap_or(false)
    };
    store.acquire(caller, paths, lease, now, &takeover)
}

/// Liveness from NEEDLE heartbeats and local processes. Bead state is not
/// consulted (returns `None`); callers with a bead store wrap this.
#[derive(Debug, Clone)]
pub struct LocalHolderStatus {
    heartbeats_dir: Option<std::path::PathBuf>,
    heartbeat_ttl: Duration,
    host: String,
}

impl LocalHolderStatus {
    pub fn new(heartbeats_dir: Option<&Path>, heartbeat_ttl: Duration) -> Self {
        Self {
            heartbeats_dir: heartbeats_dir.map(Path::to_path_buf),
            heartbeat_ttl,
            host: gethostname::gethostname().to_string_lossy().into_owned(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

impl HolderStatus for LocalHolderStatus {
    fn liveness(&self, holder: &MarkerRecord) -> Liveness {
        let fresh_heartbeat = self.heartbeats_dir.as_deref().is_some_and(|dir| {
            crate::health::HealthMonitor::read_all_heartbeats(dir)
                .unwrap_or_default()
                .iter()
                .any(|hb| {
                    (hb.worker_id == holder.worker_id || hb.qualified_id == holder.worker_id)
                        && !crate::health::HealthMonitor::is_stale(hb, self.heartbeat_ttl)
                })
        });
        if fresh_heartbeat {
            return Liveness::Live;
        }
        if holder.host != self.host {
            return Liveness::Unknown;
        }
        if crate::registry::is_pid_alive(holder.pid) {
            Liveness::Live
        } else {
            Liveness::Dead
        }
    }

    fn bead_state(&self, _bead_id: &str) -> Option<BeadState> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_contention::store::WriteIntent;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    const LEASE: Duration = Duration::from_secs(120);

    struct Fixture {
        liveness: HashMap<String, Liveness>,
        beads: HashMap<String, BeadState>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                liveness: HashMap::new(),
                beads: HashMap::new(),
            }
        }
        fn with_live(mut self, worker: &str, live: Liveness) -> Self {
            self.liveness.insert(worker.to_string(), live);
            self
        }
        fn with_bead(mut self, bead: &str, state: BeadState) -> Self {
            self.beads.insert(bead.to_string(), state);
            self
        }
    }

    impl HolderStatus for Fixture {
        fn liveness(&self, holder: &MarkerRecord) -> Liveness {
            self.liveness
                .get(&holder.worker_id)
                .copied()
                .unwrap_or(Liveness::Unknown)
        }
        fn bead_state(&self, bead_id: &str) -> Option<BeadState> {
            self.beads.get(bead_id).cloned()
        }
    }

    fn participant(worker: &str, attempt: &str, bead: &str) -> Participant {
        Participant {
            bead_id: Some(bead.into()),
            attempt_id: Some(attempt.into()),
            session_id: None,
            worker_id: worker.into(),
            host: "codinghome".into(),
            pid: 4242,
        }
    }

    fn t0() -> DateTime<Utc> {
        "2026-10-07T12:00:00Z".parse().unwrap()
    }

    fn setup() -> (tempfile::TempDir, MarkerStore) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        let store = MarkerStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn hold(store: &MarkerStore, who: &Participant, path: &str) {
        let never = |_: &str, _: &MarkerRead| false;
        let outcome = store
            .acquire(
                who,
                &[PathIntent {
                    path: PathBuf::from(path),
                    intent: WriteIntent::Modify,
                }],
                LEASE,
                t0(),
                &never,
            )
            .unwrap();
        assert!(matches!(outcome, AcquireOutcome::Acquired(_)));
    }

    #[test]
    fn free_and_same_attempt() {
        let (_dir, store) = setup();
        let me = participant("w1", "a1", "needle-1");
        assert_eq!(
            assess(&store, "src/lib.rs", Some(&me), &Fixture::new(), t0()).unwrap(),
            Assessment::Free
        );
        hold(&store, &me, "src/lib.rs");
        let a = assess(&store, "src/lib.rs", Some(&me), &Fixture::new(), t0()).unwrap();
        assert_eq!(a.kind(), "same_attempt");
        assert!(a.permits_write());
    }

    #[test]
    fn live_other_holder_is_active_even_past_its_lease() {
        let (_dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        let live = Fixture::new().with_live("w1", Liveness::Live);
        let a = assess(&store, "src/lib.rs", Some(&me), &live, t0()).unwrap();
        assert!(matches!(
            a,
            Assessment::ActiveOther {
                lease_overdue: false,
                ..
            }
        ));
        assert!(!a.permits_write());
        let late = t0() + chrono::Duration::seconds(600);
        let a = assess(&store, "src/lib.rs", Some(&me), &live, late).unwrap();
        assert!(
            matches!(
                a,
                Assessment::ActiveOther {
                    lease_overdue: true,
                    ..
                }
            ),
            "a live holder past its lease is a warning, not authority loss"
        );
    }

    #[test]
    fn unknown_liveness_trusts_the_lease_only_until_it_expires() {
        let (_dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        let unknown = Fixture::new();
        assert_eq!(
            assess(&store, "src/lib.rs", Some(&me), &unknown, t0())
                .unwrap()
                .kind(),
            "active_other"
        );
        let late = t0() + chrono::Duration::seconds(600);
        assert_eq!(
            assess(&store, "src/lib.rs", Some(&me), &unknown, late)
                .unwrap()
                .kind(),
            "stale_unchanged"
        );
    }

    #[test]
    fn dead_holder_with_unchanged_file_is_stale_unchanged_even_if_only_touched() {
        let (dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        // Rewrite identical content and move the mtime explicitly: the
        // timestamp changes, the content identity does not.
        fs::write(dir.path().join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(dir.path().join("src/lib.rs"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        let dead = Fixture::new().with_live("w1", Liveness::Dead);
        let a = assess(&store, "src/lib.rs", Some(&me), &dead, t0()).unwrap();
        assert!(
            matches!(
                a,
                Assessment::StaleUnchanged {
                    file: FileState::Unchanged {
                        mtime_changed: true
                    },
                    ..
                }
            ),
            "timestamp is never the sole proof of modification: {a:?}"
        );
        assert!(a.reclaimable() && a.permits_write());
    }

    #[test]
    fn dead_holder_with_changed_file_is_stale_modified_and_preserved() {
        let (dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn one() { half_done(); }\n",
        )
        .unwrap();
        let dead = Fixture::new().with_live("w1", Liveness::Dead);
        let a = assess(&store, "src/lib.rs", Some(&me), &dead, t0()).unwrap();
        assert!(matches!(
            a,
            Assessment::StaleModified {
                file: FileState::ContentChanged,
                ..
            }
        ));
        assert!(!a.reclaimable() && !a.permits_write());
        assert_eq!(
            a.holder().unwrap().bead_id.as_deref(),
            Some("needle-1"),
            "routes to the owning bead"
        );
    }

    #[test]
    fn created_and_deleted_files_count_as_modified() {
        let (dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        let never = |_: &str, _: &MarkerRead| false;
        store
            .acquire(
                &holder,
                &[PathIntent {
                    path: PathBuf::from("src/new.rs"),
                    intent: WriteIntent::Create,
                }],
                LEASE,
                t0(),
                &never,
            )
            .unwrap();
        hold(&store, &holder, "src/lib.rs");
        fs::write(dir.path().join("src/new.rs"), "// new\n").unwrap();
        fs::remove_file(dir.path().join("src/lib.rs")).unwrap();
        let dead = Fixture::new().with_live("w1", Liveness::Dead);
        assert!(matches!(
            assess(&store, "src/new.rs", Some(&me), &dead, t0()).unwrap(),
            Assessment::StaleModified {
                file: FileState::Created,
                ..
            }
        ));
        assert!(matches!(
            assess(&store, "src/lib.rs", Some(&me), &dead, t0()).unwrap(),
            Assessment::StaleModified {
                file: FileState::Deleted,
                ..
            }
        ));
    }

    #[test]
    fn verified_closed_bead_is_reclaimable_only_when_unchanged() {
        let (dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        let closed = Fixture::new()
            .with_live("w1", Liveness::Live)
            .with_bead("needle-1", BeadState::Closed);
        let a = assess(&store, "src/lib.rs", Some(&me), &closed, t0()).unwrap();
        assert_eq!(a.kind(), "verified_closed");
        assert!(a.reclaimable());
        fs::write(dir.path().join("src/lib.rs"), "pub fn two() {}\n").unwrap();
        let a = assess(&store, "src/lib.rs", Some(&me), &closed, t0()).unwrap();
        assert!(
            !a.reclaimable(),
            "closed bead + changed file needs a follow-up inspection"
        );
    }

    #[test]
    fn corrupt_or_contradictory_markers_are_ambiguous() {
        let (dir, store) = setup();
        let me = participant("w2", "a2", "needle-2");
        fs::create_dir_all(dir.path().join(".needle/locks/src")).unwrap();
        fs::write(dir.path().join(".needle/locks/src/lib.rs.lock"), b"garbage").unwrap();
        let a = assess(&store, "src/lib.rs", Some(&me), &Fixture::new(), t0()).unwrap();
        assert_eq!(a.kind(), "ambiguous");
        assert!(!a.permits_write() && !a.reclaimable());

        fs::remove_file(dir.path().join(".needle/locks/src/lib.rs.lock")).unwrap();
        let holder = participant("w1", "a1", "needle-1");
        hold(&store, &holder, "src/lib.rs");
        let contradictory = Fixture::new().with_live("w1", Liveness::Live).with_bead(
            "needle-1",
            BeadState::InProgress {
                attempt_id: Some("a9".into()),
            },
        );
        let a = assess(&store, "src/lib.rs", Some(&me), &contradictory, t0()).unwrap();
        assert_eq!(a.kind(), "ambiguous");
    }

    #[test]
    fn superseded_attempt_with_unknown_liveness_is_stale() {
        let (_dir, store) = setup();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-1");
        hold(&store, &holder, "src/lib.rs");
        let superseded = Fixture::new().with_bead(
            "needle-1",
            BeadState::InProgress {
                attempt_id: Some("a2".into()),
            },
        );
        assert_eq!(
            assess(&store, "src/lib.rs", Some(&me), &superseded, t0())
                .unwrap()
                .kind(),
            "stale_unchanged"
        );
    }

    #[test]
    fn reclaim_takes_over_stale_unchanged_atomically_but_never_modified_or_active() {
        let (dir, store) = setup();
        fs::write(dir.path().join("src/other.rs"), "pub fn other() {}\n").unwrap();
        let holder = participant("w1", "a1", "needle-1");
        let me = participant("w2", "a2", "needle-2");
        hold(&store, &holder, "src/lib.rs");
        hold(&store, &holder, "src/other.rs");
        let dead = Fixture::new().with_live("w1", Liveness::Dead);
        let request = |path: &str| PathIntent {
            path: PathBuf::from(path),
            intent: WriteIntent::Modify,
        };

        // other.rs modified by the dead holder: the multi-path request fails as a
        // whole and lib.rs is NOT taken over.
        fs::write(
            dir.path().join("src/other.rs"),
            "pub fn other() { wip(); }\n",
        )
        .unwrap();
        let outcome = reclaim_or_acquire(
            &store,
            &me,
            &[request("src/lib.rs"), request("src/other.rs")],
            LEASE,
            &dead,
            t0(),
        )
        .unwrap();
        let AcquireOutcome::Conflicts(conflicts) = outcome else {
            panic!("modified file must block the set");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "src/other.rs");
        let Some(MarkerRead::Valid(still)) = store.read("src/lib.rs").unwrap() else {
            panic!("lib.rs marker must remain");
        };
        assert_eq!(still.attempt_id.as_deref(), Some("a1"));

        // lib.rs alone is stale-unchanged: reclaimed.
        let outcome =
            reclaim_or_acquire(&store, &me, &[request("src/lib.rs")], LEASE, &dead, t0()).unwrap();
        let AcquireOutcome::Acquired(records) = outcome else {
            panic!("stale unchanged marker must be reclaimed");
        };
        assert_eq!(records[0].attempt_id.as_deref(), Some("a2"));

        // A live holder is never reclaimed.
        let them = participant("w3", "a3", "needle-3");
        let live = Fixture::new().with_live("w2", Liveness::Live);
        assert!(matches!(
            reclaim_or_acquire(&store, &them, &[request("src/lib.rs")], LEASE, &live, t0())
                .unwrap(),
            AcquireOutcome::Conflicts(_)
        ));
    }

    #[test]
    fn local_status_reads_this_hosts_processes() {
        let status = LocalHolderStatus::new(None, Duration::from_secs(60));
        let mut record = MarkerRecord {
            schema_version: crate::file_contention::store::SCHEMA_VERSION,
            path: "src/lib.rs".into(),
            bead_id: None,
            attempt_id: None,
            session_id: Some("s".into()),
            worker_id: "interactive".into(),
            host: status.host().to_string(),
            pid: std::process::id(),
            intent: WriteIntent::Modify,
            claimed_at: t0(),
            last_renewed_at: t0(),
            lease_expires_at: t0(),
            baseline: Baseline {
                existed: false,
                mtime_ns: None,
                size: None,
                content_sha256: None,
            },
        };
        assert_eq!(
            status.liveness(&record),
            Liveness::Live,
            "this test process is alive"
        );
        record.pid = u32::MAX - 7;
        assert_eq!(status.liveness(&record), Liveness::Dead);
        record.host = "some-other-host".into();
        assert_eq!(
            status.liveness(&record),
            Liveness::Unknown,
            "another host's pid proves nothing"
        );
    }
}
