use std::collections::HashSet;
use std::path::PathBuf;

use super::*;

struct MockInspector {
    live_pids: HashSet<u32>,
}

impl ProcessInspector for MockInspector {
    fn tree_has_live_process(&self, root_pid: u32) -> bool {
        self.live_pids.contains(&root_pid)
    }
}

fn session(name: &str, pid: Option<u32>) -> TmuxSession {
    TmuxSession {
        name: name.to_string(),
        created: "20260922T120000".to_string(),
        status: "detached".to_string(),
        pid,
    }
}

fn process(pid: u32, workspace: Option<&str>, agent: Option<&str>) -> DiscoveredProcess {
    DiscoveredProcess {
        pid,
        workspace: workspace.map(PathBuf::from),
        agent: agent.map(str::to_string),
        identifier: None,
        cmdline: "needle run".to_string(),
    }
}

fn worker(id: &str, pid: u32) -> WorkerEntry {
    WorkerEntry {
        id: id.to_string(),
        pid,
        workspace: PathBuf::from("/workspace"),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: chrono::Utc::now(),
        beads_processed: 0,
        beads_completed: 0,
        config_reload_generation: 0,
        state: None,
    }
}

#[test]
fn reconciliation_separates_live_and_stale_sessions() {
    let sessions = vec![
        session("needle-claude-live", Some(101)),
        session("needle-claude-stale", Some(202)),
        session("needle-claude-no-pane", None),
    ];
    let inspector = MockInspector {
        live_pids: HashSet::from([101]),
    };

    let (live, stale) = reconcile_tmux_sessions(&sessions, &inspector);

    assert_eq!(
        live.iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        vec!["needle-claude-live"]
    );
    assert_eq!(
        stale
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        vec!["needle-claude-stale", "needle-claude-no-pane"]
    );
}

#[test]
fn reconciliation_keeps_live_processes_even_without_registry_metadata() {
    let discovered = vec![
        process(303, Some("/workspace/live"), Some("claude")),
        // A valid `needle run` can use configured defaults instead of passing
        // --workspace or --agent. Optional metadata must not make it vanish.
        process(404, None, None),
    ];
    let registered = HashSet::from([303]);

    let invisible = unregistered_processes(&discovered, &registered);

    assert_eq!(invisible.len(), 1);
    assert_eq!(invisible[0].pid, 404);
    assert!(invisible[0].workspace.is_none());
    assert!(invisible[0].agent.is_none());
}

#[test]
fn reconciliation_does_not_confuse_pid_metadata_with_session_liveness() {
    const LIVE_WORKER_PID: u32 = u32::MAX - 2;
    const UNREGISTERED_PID: u32 = u32::MAX - 1;
    const STALE_WORKER_PID: u32 = u32::MAX;

    let sessions = vec![session("needle-claude-wrapper", Some(505))];
    let inspector = MockInspector {
        live_pids: HashSet::from([505]),
    };
    let discovered = vec![
        process(LIVE_WORKER_PID, Some("/workspace/live"), Some("claude")),
        process(
            UNREGISTERED_PID,
            Some("/workspace/unregistered"),
            Some("claude"),
        ),
    ];
    let registered = vec![
        worker("wrapper", LIVE_WORKER_PID),
        worker("stale", STALE_WORKER_PID),
    ];

    let (live, stale) = reconcile_tmux_sessions(&sessions, &inspector);
    let unregistered = unregistered_processes(&discovered, &HashSet::from([LIVE_WORKER_PID]));
    let process_reconciliation = reconcile_process_registry(&discovered, &registered);

    assert_eq!(live.len(), 1, "a live pane tree is an active session");
    assert!(stale.is_empty());
    assert_eq!(
        unregistered[0].pid, UNREGISTERED_PID,
        "direct workers remain visible"
    );
    assert_eq!(
        process_reconciliation
            .live_registered
            .iter()
            .map(|worker| worker.id.as_str())
            .collect::<Vec<_>>(),
        vec!["wrapper"]
    );
    assert_eq!(
        process_reconciliation
            .stale_registered
            .iter()
            .map(|worker| worker.id.as_str())
            .collect::<Vec<_>>(),
        vec!["stale"]
    );
    assert_eq!(
        process_reconciliation
            .unregistered
            .iter()
            .map(|process| process.pid)
            .collect::<Vec<_>>(),
        vec![UNREGISTERED_PID]
    );

    let cleanup_sessions = vec![
        session("needle-wrapper", Some(505)),
        session("needle-claude-stale", Some(707)),
    ];
    let live_worker_ids = live_registered_worker_ids(&discovered, &registered);
    let inspector = MockInspector {
        live_pids: HashSet::new(),
    };
    let cleanup_targets = filter_sessions_for_cleanup_with_live_workers(
        &cleanup_sessions,
        &inspector,
        &live_worker_ids,
        false,
        &None,
    );
    assert_eq!(
        cleanup_targets,
        vec!["needle-claude-stale".to_string()],
        "bare cleanup must keep the registered worker whose PID is live"
    );
}

#[test]
fn bare_cleanup_preserves_live_registered_worker_and_removes_orphan() {
    const LIVE_WORKER_PID: u32 = u32::MAX - 1;
    const ORPHAN_WORKER_PID: u32 = u32::MAX;

    let sessions = vec![
        session("needle-claude-live", Some(101)),
        session("needle-claude-orphan", Some(202)),
    ];
    // Both tmux panes have live processes, but the orphan's process is only a
    // shell. Bare cleanup must use NEEDLE process discovery plus the registry
    // PID, rather than treating any live pane process as a live worker.
    let inspector = MockInspector {
        live_pids: HashSet::from([101, 202]),
    };
    let discovered = vec![process(
        LIVE_WORKER_PID,
        Some("/workspace/live"),
        Some("claude"),
    )];
    let registered = vec![
        worker("claude-live", LIVE_WORKER_PID),
        worker("claude-orphan", ORPHAN_WORKER_PID),
    ];
    let live_worker_ids = live_registered_worker_ids(&discovered, &registered);

    let cleanup_targets = filter_sessions_for_cleanup_with_live_workers(
        &sessions,
        &inspector,
        &live_worker_ids,
        false,
        &None,
    );

    assert_eq!(
        cleanup_targets,
        vec!["needle-claude-orphan".to_string()],
        "bare cleanup must keep the session with a live registered NEEDLE PID and remove the orphan"
    );
}
