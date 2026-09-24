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

    let cleanup_targets = filter_sessions_for_cleanup_impl(&sessions, &inspector, false, &None);
    assert_eq!(
        cleanup_targets,
        vec![
            "needle-claude-stale".to_string(),
            "needle-claude-no-pane".to_string(),
        ],
        "bare cleanup must preserve live sessions and select reconciled orphans"
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
    let sessions = vec![session("needle-claude-wrapper", Some(505))];
    let inspector = MockInspector {
        live_pids: HashSet::from([505]),
    };
    let discovered = vec![process(606, Some("/workspace"), Some("claude"))];

    let (live, stale) = reconcile_tmux_sessions(&sessions, &inspector);
    let unregistered = unregistered_processes(&discovered, &HashSet::new());

    assert_eq!(live.len(), 1, "a live pane tree is an active session");
    assert!(stale.is_empty());
    assert_eq!(unregistered[0].pid, 606, "direct workers remain visible");
}
