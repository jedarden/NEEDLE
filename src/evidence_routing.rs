//! Evidence-based adapter selection (plan section 4.4 step 4 / ledger N-T18).
//!
//! Static routing (`agent.routing.rules`) matches on a model name and never
//! learns. This module chooses among *already configured* candidate
//! adapters by what the attempt ledger says about them — verified success
//! per attempt, then cost per verified success — inside the L1 envelope of
//! plan section 5.7: it selects among approved variants, records the
//! exposure, and cannot create a new adapter or widen anything.
//!
//! Guardrails that keep this from being a random walk:
//!
//! - **Evidence floor.** An adapter with fewer than `min_attempts` ledger
//!   rows in the window cannot be chosen as best; static routing remains the
//!   default when nothing clears the floor.
//! - **Minimum improvement.** Switching away from the statically routed
//!   adapter needs the best candidate to beat it by `min_improvement` in
//!   verified success rate, so the fleet does not flap on noise.
//! - **Bounded exploration.** With probability `exploration_share` an
//!   eligible non-best candidate is dispatched instead, so evidence keeps
//!   accruing for every candidate; the choice is recorded as explored.
//! - **Freeze.** A gate-degraded workspace or a degraded adapter (N-T21,
//!   N-T23) takes the static default: infrastructure noise must not move
//!   routing.
//!
//! The evidence comes from `attempt.resolved` rows in the local telemetry
//! logs (bounded by file date, read once per `refresh_secs`), never from an
//! agent's own report.
//!
//! **Scope (N-T48, ADR-030 decision 4).** With `workspace_scope` on, the
//! bead's own workspace evidence decides first; below the floor the fleet's
//! evidence decides, and below that the static default, and the receipt
//! records which scope decided. A workspace where every candidate verifies
//! below `workspace_poor_threshold` is signalled
//! (`workspace.adapter_evidence_poor`), never routed on.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::EvidenceRoutingConfig;

/// What the ledger says about one adapter in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdapterEvidence {
    pub adapter: String,
    /// Ledger rows (attempts).
    pub attempts: u64,
    /// Rows resolved `verified_success`.
    pub verified: u64,
    /// Rows resolved `infrastructure_failure` (excluded from the rate).
    pub infrastructure: u64,
    /// Rows resolved `decomposed` (excluded from the rate): a split is
    /// neither a win nor a loss for the adapter (ADR-030).
    #[serde(default)]
    pub decomposed: u64,
    /// Sum of `estimated_cost_usd` over rows that reported one.
    pub cost_usd: f64,
    /// Rows that reported a cost (denominator for cost per success).
    pub costed: u64,
}

impl AdapterEvidence {
    /// Attempts that were the adapter's to win: everything but
    /// infrastructure failures and decompositions.
    pub fn judged(&self) -> u64 {
        self.attempts
            .saturating_sub(self.infrastructure)
            .saturating_sub(self.decomposed)
    }

    /// Fold one `attempt.resolved` row's `data` into this evidence.
    ///
    /// The one reading of a ledger row for routing: [`evidence_from_logs`]
    /// and the worker's cached ledger snapshot both call it, so a new
    /// resolution class is classified in one place. A decomposed row still
    /// adds its cost — splitting is spend without a verified success.
    pub fn observe_row(&mut self, row: &serde_json::Value) {
        self.attempts += 1;
        match row.get("outcome").and_then(|v| v.as_str()) {
            Some("verified_success") => self.verified += 1,
            Some("infrastructure_failure") => self.infrastructure += 1,
            Some(crate::attempt_accounting::DECOMPOSED) => self.decomposed += 1,
            _ => {}
        }
        if let Some(cost) = row.get("estimated_cost_usd").and_then(|v| v.as_f64()) {
            self.cost_usd += cost;
            self.costed += 1;
        }
    }

    /// Verified successes over judged attempts.
    pub fn success_rate(&self) -> Option<f64> {
        let judged = self.judged();
        if judged == 0 {
            None
        } else {
            Some(self.verified as f64 / judged as f64)
        }
    }

    /// Reported cost per verified success, when both are known.
    pub fn cost_per_success(&self) -> Option<f64> {
        if self.verified == 0 || self.costed == 0 {
            None
        } else {
            Some(self.cost_usd / self.verified as f64)
        }
    }

    /// The fields a telemetry receipt carries for this evidence.
    pub fn receipt(&self) -> serde_json::Value {
        serde_json::json!({
            "adapter": self.adapter,
            "attempts": self.attempts,
            "judged": self.judged(),
            "verified": self.verified,
            "success_rate": self.success_rate(),
            "cost_per_success": self.cost_per_success(),
        })
    }
}

/// One routing decision, with everything needed to explain it.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub adapter: String,
    pub reason: String,
    pub explored: bool,
    /// Evidence for every candidate considered (for the telemetry receipt).
    pub considered: Vec<AdapterEvidence>,
}

/// Choose an adapter. `default` is the statically routed adapter; `roll` is a
/// uniform sample in `[0, 1)` supplied by the caller (tests pass a constant).
pub fn choose(
    default: &str,
    config: &EvidenceRoutingConfig,
    evidence: &HashMap<String, AdapterEvidence>,
    degraded_adapters: &HashSet<String>,
    frozen: Option<&str>,
    roll: f64,
) -> Choice {
    let candidates = candidate_names(default, config);
    let considered = considered_from(&candidates, evidence);

    if let Some(why) = frozen {
        return Choice {
            adapter: default.to_string(),
            reason: format!("frozen:{why}"),
            explored: false,
            considered,
        };
    }

    let eligible: Vec<&AdapterEvidence> = considered
        .iter()
        .filter(|e| !degraded_adapters.contains(&e.adapter))
        .collect();
    if eligible.is_empty() {
        return Choice {
            adapter: default.to_string(),
            reason: "no_eligible_candidate".to_string(),
            explored: false,
            considered,
        };
    }

    // Best by success rate among those clearing the evidence floor; cost per
    // success breaks ties (lower is better).
    let mut ranked: Vec<&AdapterEvidence> = eligible
        .iter()
        .copied()
        .filter(|e| e.judged() >= config.min_attempts)
        .collect();
    ranked.sort_by(|a, b| {
        let ra = a.success_rate().unwrap_or(0.0);
        let rb = b.success_rate().unwrap_or(0.0);
        rb.partial_cmp(&ra)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ca = a.cost_per_success().unwrap_or(f64::MAX);
                let cb = b.cost_per_success().unwrap_or(f64::MAX);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.adapter.cmp(&b.adapter))
    });

    // Bounded exploration: sometimes dispatch a non-best eligible candidate
    // so its evidence keeps accruing. Uniform over the others.
    let best_name = ranked.first().map(|e| e.adapter.clone());
    if config.exploration_share > 0.0 && roll < config.exploration_share {
        let others: Vec<&AdapterEvidence> = eligible
            .iter()
            .copied()
            .filter(|e| Some(&e.adapter) != best_name.as_ref())
            .collect();
        if !others.is_empty() {
            let idx = ((roll / config.exploration_share) * others.len() as f64) as usize;
            let pick = others[idx.min(others.len() - 1)];
            return Choice {
                adapter: pick.adapter.clone(),
                reason: format!("explore:{:.2}", config.exploration_share),
                explored: true,
                considered,
            };
        }
    }

    let Some(best) = ranked.first() else {
        return Choice {
            adapter: default.to_string(),
            reason: format!("insufficient_evidence:min_attempts={}", config.min_attempts),
            explored: false,
            considered,
        };
    };
    if best.adapter == default {
        return Choice {
            adapter: default.to_string(),
            reason: "default_is_best".to_string(),
            explored: false,
            considered,
        };
    }
    // The floor protects the default too: an adapter with too few judged
    // attempts in the window has an unknown rate, not a rate of zero. Without
    // this, codex workers whose adapter had no ledger rows yet had 75 beads
    // routed onto GLM flash on 2026-09-13 "on evidence" — away from the
    // fleet's best-performing adapter (100 % verified on the beads it kept).
    let default_evidence = evidence.get(default);
    let default_judged = default_evidence.map(|e| e.judged()).unwrap_or(0);
    if !degraded_adapters.contains(default) && default_judged < config.min_attempts {
        return Choice {
            adapter: default.to_string(),
            reason: format!(
                "insufficient_evidence_for_default:{}<{}",
                default_judged, config.min_attempts
            ),
            explored: false,
            considered,
        };
    }
    let default_rate = default_evidence
        .and_then(|e| e.success_rate())
        .unwrap_or(0.0);
    let best_rate = best.success_rate().unwrap_or(0.0);
    if degraded_adapters.contains(default) || best_rate >= default_rate + config.min_improvement {
        Choice {
            adapter: best.adapter.clone(),
            reason: format!(
                "evidence:{:.2}_vs_{:.2}_over_{}",
                best_rate,
                default_rate,
                best.judged()
            ),
            explored: false,
            considered,
        }
    } else {
        Choice {
            adapter: default.to_string(),
            reason: format!(
                "below_min_improvement:{:.2}_vs_{:.2}",
                best_rate, default_rate
            ),
            explored: false,
            considered,
        }
    }
}

/// The candidate list for one decision: the configured candidates, with the
/// statically routed default first when it is not already among them.
fn candidate_names(default: &str, config: &EvidenceRoutingConfig) -> Vec<String> {
    let mut candidates: Vec<String> = config.candidates.clone();
    if !candidates.iter().any(|c| c == default) {
        candidates.insert(0, default.to_string());
    }
    candidates
}

/// Every candidate's evidence, with an empty record for a candidate the
/// ledger has not seen (an unknown rate, never a rate of zero).
fn considered_from(
    candidates: &[String],
    evidence: &HashMap<String, AdapterEvidence>,
) -> Vec<AdapterEvidence> {
    candidates
        .iter()
        .map(|name| {
            evidence
                .get(name)
                .cloned()
                .unwrap_or_else(|| AdapterEvidence {
                    adapter: name.clone(),
                    ..AdapterEvidence::default()
                })
        })
        .collect()
}

/// Which evidence decided a routing choice (ADR-030 decision 4, N-T48).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionScope {
    /// The bead's workspace had enough evidence for the default and another
    /// candidate, and that evidence decided.
    Workspace,
    /// Fleet-wide evidence decided.
    Fleet,
    /// No scope had enough evidence, or routing was frozen: the static
    /// default stands.
    Static,
}

impl DecisionScope {
    /// Wire name used in receipts and reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionScope::Workspace => "workspace",
            DecisionScope::Fleet => "fleet",
            DecisionScope::Static => "static",
        }
    }
}

/// Ledger evidence at both scopes: fleet-wide per adapter, and per
/// `(workspace, adapter)`. One [`Evidence::observe`] feeds both, so the
/// worker's cached snapshot and [`evidence_from_logs`] cannot disagree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evidence {
    /// Evidence per adapter across every workspace.
    pub fleet: HashMap<String, AdapterEvidence>,
    /// Evidence per `(workspace key, adapter)`; see [`workspace_key`].
    pub workspace: HashMap<(String, String), AdapterEvidence>,
}

impl Evidence {
    /// Evidence from a window of ledger rows (their `data` objects).
    pub fn from_rows(rows: &[serde_json::Value]) -> Self {
        let mut evidence = Self::default();
        for row in rows {
            evidence.observe(row);
        }
        evidence
    }

    /// Fold one `attempt.resolved` row into both scopes. A row without an
    /// adapter is skipped; a row without a usable workspace counts only
    /// fleet-wide.
    pub fn observe(&mut self, row: &serde_json::Value) {
        let Some(adapter) = row
            .get("adapter")
            .and_then(|v| v.as_str())
            .filter(|a| !a.is_empty())
        else {
            return;
        };
        let fresh = || AdapterEvidence {
            adapter: adapter.to_string(),
            ..AdapterEvidence::default()
        };
        self.fleet
            .entry(adapter.to_string())
            .or_insert_with(fresh)
            .observe_row(row);
        if let Some(workspace) = row
            .get("workspace")
            .and_then(|v| v.as_str())
            .and_then(workspace_key)
        {
            self.workspace
                .entry((workspace, adapter.to_string()))
                .or_insert_with(fresh)
                .observe_row(row);
        }
    }

    /// One workspace's evidence, keyed by adapter.
    pub fn in_workspace(&self, workspace: &str) -> HashMap<String, AdapterEvidence> {
        let Some(key) = workspace_key(workspace) else {
            return HashMap::new();
        };
        self.workspace
            .iter()
            .filter(|((ws, _), _)| *ws == key)
            .map(|((_, adapter), evidence)| (adapter.clone(), evidence.clone()))
            .collect()
    }

    /// Every workspace with evidence, sorted.
    pub fn workspaces(&self) -> Vec<String> {
        let keys: std::collections::BTreeSet<&String> =
            self.workspace.keys().map(|(ws, _)| ws).collect();
        keys.into_iter().cloned().collect()
    }
}

/// The key a workspace path is grouped under: the path without trailing
/// slashes. Unset (`""`) and relative-root (`"."`) workspaces carry no
/// workspace evidence.
pub fn workspace_key(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    (!trimmed.is_empty() && trimmed != ".").then(|| trimmed.to_string())
}

/// A routing choice together with the scope that decided it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedChoice {
    /// The choice; its `considered` is the deciding scope's evidence.
    pub choice: Choice,
    /// Which evidence decided.
    pub scope: DecisionScope,
    /// The workspace key the choice was made for, when one was known.
    pub workspace: Option<String>,
    /// Every candidate's evidence in that workspace (empty when workspace
    /// scope is off or no workspace was known).
    pub considered_workspace: Vec<AdapterEvidence>,
    /// Every candidate's fleet-wide evidence.
    pub considered_fleet: Vec<AdapterEvidence>,
}

/// Whether every considered candidate has enough evidence and every one
/// verifies below the poor-workspace threshold (never, when it is 0).
fn is_poor(considered: &[AdapterEvidence], config: &EvidenceRoutingConfig) -> bool {
    config.workspace_poor_threshold > 0.0
        && !considered.is_empty()
        && considered.iter().all(|e| {
            e.judged() >= config.min_attempts
                && e.success_rate().unwrap_or(0.0) < config.workspace_poor_threshold
        })
}

/// Reasons [`choose`] gives when the static default stands for lack of
/// evidence or because routing is frozen.
fn is_static_fallback(reason: &str) -> bool {
    reason.starts_with("frozen:")
        || reason.starts_with("insufficient_evidence")
        || reason == "no_eligible_candidate"
}

/// Choose an adapter from the narrowest evidence that can decide (N-T48,
/// ADR-030 decision 4).
///
/// With `workspace_scope` on and a known workspace, that workspace's
/// evidence decides when the static default and at least one other eligible
/// candidate each have `min_attempts` judged attempts there and the
/// workspace is not poor. Otherwise fleet evidence decides exactly as
/// [`choose`] always has. A choice that stayed on the default for lack of
/// evidence, or because routing is frozen, records
/// [`DecisionScope::Static`]. With workspace scope off, the choice and its
/// reason are exactly [`choose`] over fleet evidence.
pub fn choose_scoped(
    default: &str,
    config: &EvidenceRoutingConfig,
    evidence: &Evidence,
    workspace: Option<&str>,
    degraded_adapters: &HashSet<String>,
    frozen: Option<&str>,
    roll: f64,
) -> ScopedChoice {
    let candidates = candidate_names(default, config);
    let workspace = workspace.and_then(workspace_key);
    let workspace_evidence = match (&workspace, config.workspace_scope) {
        (Some(key), true) => evidence.in_workspace(key),
        _ => HashMap::new(),
    };
    let considered_workspace = if config.workspace_scope && workspace.is_some() {
        considered_from(&candidates, &workspace_evidence)
    } else {
        Vec::new()
    };
    let considered_fleet = considered_from(&candidates, &evidence.fleet);

    let floor = config.min_attempts;
    let default_judged_here = workspace_evidence
        .get(default)
        .map(|e| e.judged())
        .unwrap_or(0);
    let workspace_decides = frozen.is_none()
        && !considered_workspace.is_empty()
        && !is_poor(&considered_workspace, config)
        && default_judged_here >= floor
        && considered_workspace.iter().any(|e| {
            e.adapter != default && !degraded_adapters.contains(&e.adapter) && e.judged() >= floor
        });

    let (mut choice, deciding) = if workspace_decides {
        (
            choose(
                default,
                config,
                &workspace_evidence,
                degraded_adapters,
                frozen,
                roll,
            ),
            DecisionScope::Workspace,
        )
    } else {
        (
            choose(
                default,
                config,
                &evidence.fleet,
                degraded_adapters,
                frozen,
                roll,
            ),
            DecisionScope::Fleet,
        )
    };
    let scope = if is_static_fallback(&choice.reason) {
        DecisionScope::Static
    } else {
        deciding
    };
    if config.workspace_scope {
        choice.reason = format!("{}:{}", scope.as_str(), choice.reason);
    }
    ScopedChoice {
        choice,
        scope,
        workspace,
        considered_workspace,
        considered_fleet,
    }
}

/// A workspace where every candidate has enough evidence and every one
/// verifies below `workspace_poor_threshold`.
#[derive(Debug, Clone, PartialEq)]
pub struct PoorWorkspace {
    pub workspace: String,
    pub candidates: Vec<AdapterEvidence>,
}

/// Every poor workspace in the evidence, sorted by workspace. Empty unless
/// `workspace_scope` is on with a positive threshold and candidates. A
/// candidate with no evidence in a workspace makes it unknown, not poor.
pub fn poor_workspaces(evidence: &Evidence, config: &EvidenceRoutingConfig) -> Vec<PoorWorkspace> {
    if !config.workspace_scope
        || config.workspace_poor_threshold <= 0.0
        || config.candidates.is_empty()
    {
        return Vec::new();
    }
    evidence
        .workspaces()
        .into_iter()
        .filter_map(|workspace| {
            let candidates =
                considered_from(&config.candidates, &evidence.in_workspace(&workspace));
            is_poor(&candidates, config).then_some(PoorWorkspace {
                workspace,
                candidates,
            })
        })
        .collect()
}

/// Directory holding poor-workspace receipts:
/// `~/.needle/state/evidence_routing`.
pub fn default_state_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".needle").join("state").join("evidence_routing")
}

/// Record a poor workspace for the window containing `now`, returning
/// `Ok(true)` only for the first record in that window. The receipt is
/// created exclusively, so every worker that notices the same workspace in
/// the same window emits the signal once between them.
pub fn record_poor_workspace(
    state_dir: &Path,
    poor: &PoorWorkspace,
    window_days: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    use std::io::Write as _;

    let window_secs = 86_400 * i64::from(window_days.max(1));
    let window = now.timestamp().div_euclid(window_secs);
    let slug: String = poor
        .workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = state_dir.join(format!("{slug}--window-{window}.poor.json"));
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("failed to create {}", path.display())),
    };
    let receipt = serde_json::json!({
        "schema_version": 1,
        "workspace": poor.workspace,
        "window_days": window_days,
        "recorded_at": now.to_rfc3339(),
        "candidates": poor.candidates.iter().map(AdapterEvidence::receipt).collect::<Vec<_>>(),
    });
    file.write_all(serde_json::to_string_pretty(&receipt)?.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

/// Aggregate `attempt.resolved` rows from the telemetry log directory for
/// the last `window_days`, keyed by adapter: the fleet scope of
/// [`Evidence`].
///
/// Reads only files whose date suffix falls in the window and only lines
/// that mention the event, so a 100 GB log directory costs seconds, not
/// minutes. Test and transform logs are skipped.
pub fn evidence_from_logs(log_dir: &Path, window_days: u32) -> HashMap<String, AdapterEvidence> {
    Evidence::from_rows(&ledger_rows(log_dir, window_days)).fleet
}

/// The `data` objects of every `attempt.resolved` row in the window.
pub fn ledger_rows(log_dir: &Path, window_days: u32) -> Vec<serde_json::Value> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(window_days));
    let cutoff_day = cutoff.format("%Y-%m-%d").to_string();
    let cutoff_ts = cutoff.to_rfc3339();
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return rows;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".jsonl")
            || name.ends_with(".agent.jsonl")
            || name.contains("test")
            || name.starts_with("background-update")
        {
            continue;
        }
        // `<worker>-<session>-YYYY-MM-DD.jsonl`: the date suffix bounds the file.
        let day = name
            .strip_suffix(".jsonl")
            .and_then(|s| s.rsplit_once('-').map(|(head, _)| head))
            .and_then(|s| s.rsplit_once('-').map(|(head, _)| head))
            .and_then(|s| s.get(s.len().saturating_sub(4)..))
            .map(|_| &name[name.len() - 16..name.len() - 6]);
        if let Some(day) = day {
            if day.len() == 10 && day < cutoff_day.as_str() {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if !line.contains("\"attempt.resolved\"") {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if event.get("event_type").and_then(|v| v.as_str()) != Some("attempt.resolved") {
                continue;
            }
            if event
                .get("timestamp")
                .and_then(|v| v.as_str())
                .is_some_and(|ts| ts < cutoff_ts.as_str())
            {
                continue;
            }
            if let Some(data) = event.get("data").cloned() {
                rows.push(data);
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(adapter: &str, attempts: u64, verified: u64, cost: f64) -> AdapterEvidence {
        AdapterEvidence {
            adapter: adapter.into(),
            attempts,
            verified,
            infrastructure: 0,
            decomposed: 0,
            cost_usd: cost,
            costed: attempts,
        }
    }

    fn cfg() -> EvidenceRoutingConfig {
        EvidenceRoutingConfig {
            enabled: true,
            candidates: vec!["a".into(), "b".into()],
            min_attempts: 20,
            exploration_share: 0.1,
            min_improvement: 0.05,
            window_days: 7,
            refresh_secs: 600,
            workspace_scope: false,
            workspace_poor_threshold: 0.0,
        }
    }

    fn map(items: Vec<AdapterEvidence>) -> HashMap<String, AdapterEvidence> {
        items.into_iter().map(|e| (e.adapter.clone(), e)).collect()
    }

    #[test]
    fn insufficient_evidence_keeps_the_static_default() {
        let evidence = map(vec![ev("a", 5, 5, 1.0), ev("b", 3, 0, 1.0)]);
        let c = choose("a", &cfg(), &evidence, &HashSet::new(), None, 0.5);
        assert_eq!(c.adapter, "a");
        assert!(c.reason.starts_with("insufficient_evidence"));
        assert!(!c.explored);
        assert_eq!(c.considered.len(), 2);

        // The floor protects the default too: no ledger rows (or too few)
        // means an unknown rate, not zero, so a candidate with evidence must
        // not pull work away from it. On 2026-09-13 this moved 75 codex
        // beads onto GLM flash before the guard existed.
        let evidence = map(vec![ev("b", 40, 30, 30.0)]);
        let c = choose("codex", &cfg(), &evidence, &HashSet::new(), None, 0.5);
        assert_eq!(c.adapter, "codex");
        assert!(
            c.reason
                .starts_with("insufficient_evidence_for_default:0<20"),
            "{}",
            c.reason
        );
        let thin = map(vec![ev("codex", 5, 5, 0.0), ev("b", 40, 30, 30.0)]);
        let c = choose("codex", &cfg(), &thin, &HashSet::new(), None, 0.5);
        assert_eq!(c.adapter, "codex");
        // A degraded default still yields to an eligible candidate.
        let degraded: HashSet<String> = ["codex".to_string()].into_iter().collect();
        let c = choose("codex", &cfg(), &evidence, &degraded, None, 0.5);
        assert_eq!(c.adapter, "b");
    }

    #[test]
    fn a_clearly_better_candidate_wins_and_a_marginal_one_does_not() {
        let evidence = map(vec![ev("a", 40, 20, 40.0), ev("b", 40, 32, 30.0)]);
        let c = choose("a", &cfg(), &evidence, &HashSet::new(), None, 0.5);
        assert_eq!(c.adapter, "b");
        assert!(c.reason.starts_with("evidence:"), "{}", c.reason);

        let marginal = map(vec![ev("a", 40, 20, 40.0), ev("b", 40, 21, 30.0)]);
        let c = choose("a", &cfg(), &marginal, &HashSet::new(), None, 0.5);
        assert_eq!(c.adapter, "a");
        assert!(c.reason.starts_with("below_min_improvement"));
    }

    #[test]
    fn degraded_and_frozen_never_move_routing_toward_noise() {
        let evidence = map(vec![ev("a", 40, 20, 40.0), ev("b", 40, 32, 30.0)]);
        let degraded: HashSet<String> = ["b".to_string()].into_iter().collect();
        let c = choose("a", &cfg(), &evidence, &degraded, None, 0.5);
        assert_eq!(c.adapter, "a");
        let c = choose(
            "a",
            &cfg(),
            &evidence,
            &HashSet::new(),
            Some("workspace_degraded"),
            0.5,
        );
        assert_eq!(c.adapter, "a");
        assert_eq!(c.reason, "frozen:workspace_degraded");
        // A degraded default yields to any eligible candidate with evidence.
        let degraded: HashSet<String> = ["a".to_string()].into_iter().collect();
        let c = choose("a", &cfg(), &evidence, &degraded, None, 0.5);
        assert_eq!(c.adapter, "b");
    }

    #[test]
    fn exploration_is_bounded_by_the_share() {
        let evidence = map(vec![ev("a", 40, 30, 40.0), ev("b", 40, 10, 30.0)]);
        let c = choose("a", &cfg(), &evidence, &HashSet::new(), None, 0.05);
        assert!(c.explored);
        assert_eq!(c.adapter, "b");
        let c = choose("a", &cfg(), &evidence, &HashSet::new(), None, 0.5);
        assert!(!c.explored);
        assert_eq!(c.adapter, "a");
    }

    #[test]
    fn nt46_splits_are_attempts_but_never_verified_or_judged() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let split = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event_type": "attempt.resolved",
            "worker_id": "w",
            "data": {"adapter": "a", "outcome": "decomposed", "estimated_cost_usd": 0.5}
        })
        .to_string();
        std::fs::write(
            dir.path().join(format!("w-abcd1234-{today}.jsonl")),
            vec![split; 10].join("\n"),
        )
        .unwrap();
        let evidence = evidence_from_logs(dir.path(), 7);
        let a = evidence.get("a").expect("adapter a");
        assert_eq!((a.attempts, a.verified, a.judged()), (10, 0, 0));
        assert_eq!(a.decomposed, 10);
        assert_eq!(a.success_rate(), None, "splits are no evidence either way");
        assert_eq!(a.costed, 10, "a split is still spend");
    }

    #[test]
    fn infrastructure_failures_do_not_count_against_an_adapter() {
        let e = AdapterEvidence {
            adapter: "a".into(),
            attempts: 30,
            verified: 15,
            infrastructure: 10,
            decomposed: 0,
            cost_usd: 0.0,
            costed: 0,
        };
        assert_eq!(e.judged(), 20);
        assert_eq!(e.success_rate(), Some(0.75));
        assert_eq!(e.cost_per_success(), None);
    }

    #[test]
    fn evidence_from_logs_reads_only_windowed_ledger_rows() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let row = |adapter: &str, outcome: &str| {
            serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event_type": "attempt.resolved",
                "worker_id": "w",
                "data": {"adapter": adapter, "outcome": outcome, "estimated_cost_usd": 0.5}
            })
            .to_string()
        };
        std::fs::write(
            dir.path().join(format!("w-abcd1234-{today}.jsonl")),
            format!(
                "{}\n{}\n{{\"event_type\":\"heartbeat.emitted\"}}\n",
                row("a", "verified_success"),
                row("a", "infrastructure_failure")
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("w-abcd1234-2020-01-01.jsonl"),
            row("a", "verified_success"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(format!("test-worker-{today}.jsonl")),
            row("a", "verified_success"),
        )
        .unwrap();
        let evidence = evidence_from_logs(dir.path(), 7);
        let a = evidence.get("a").expect("adapter a");
        assert_eq!(a.attempts, 2);
        assert_eq!(a.verified, 1);
        assert_eq!(a.infrastructure, 1);
        assert_eq!(a.costed, 2);
    }

    // ── N-T48: workspace-scoped evidence ──

    /// Ledger rows shaped like the 2026-09-12..14 evidence behind N-T48
    /// (codex 33/34 and flash 9/24 in reddit-media-player; flash 11/15 and
    /// glm 4/48 in pdftract), plus a thin workspace and a poor one.
    fn replay_rows() -> Vec<serde_json::Value> {
        let mut rows = Vec::new();
        let mut push = |workspace: &str, adapter: &str, verified: u64, attempts: u64| {
            for i in 0..attempts {
                rows.push(serde_json::json!({
                    "workspace": workspace,
                    "adapter": adapter,
                    "outcome": if i < verified { "verified_success" } else { "work_failure" },
                }));
            }
        };
        push("/home/coding/reddit-media-player", "codex", 33, 34);
        push("/home/coding/reddit-media-player", "flash", 9, 24);
        push("/home/coding/pdftract", "flash", 11, 15);
        push("/home/coding/pdftract", "glm", 4, 48);
        // A trailing slash is the same workspace.
        push("/home/coding/LOOM/", "flash", 3, 4);
        push("/home/coding/LOOM", "glm", 2, 5);
        push("/home/coding/NEEDLE", "flash", 0, 22);
        push("/home/coding/NEEDLE", "glm", 1, 12);
        rows
    }

    fn scoped_cfg() -> EvidenceRoutingConfig {
        EvidenceRoutingConfig {
            candidates: vec!["flash".into(), "glm".into(), "codex".into()],
            min_attempts: 10,
            exploration_share: 0.0,
            workspace_scope: true,
            workspace_poor_threshold: 0.2,
            ..cfg()
        }
    }

    #[test]
    fn nt48_workspace_evidence_routes_each_workspace_to_its_own_best_adapter() {
        let evidence = Evidence::from_rows(&replay_rows());
        let none = HashSet::new();

        let rmp = choose_scoped(
            "flash",
            &scoped_cfg(),
            &evidence,
            Some("/home/coding/reddit-media-player"),
            &none,
            None,
            0.5,
        );
        assert_eq!(rmp.choice.adapter, "codex");
        assert_eq!(rmp.scope, DecisionScope::Workspace);
        assert!(
            rmp.choice.reason.starts_with("workspace:evidence:"),
            "{}",
            rmp.choice.reason
        );
        assert_eq!(
            rmp.workspace.as_deref(),
            Some("/home/coding/reddit-media-player")
        );

        let pdftract = choose_scoped(
            "glm",
            &scoped_cfg(),
            &evidence,
            Some("/home/coding/pdftract/"),
            &none,
            None,
            0.5,
        );
        assert_eq!(pdftract.choice.adapter, "flash");
        assert_eq!(pdftract.scope, DecisionScope::Workspace);

        // Below the floor in LOOM: fleet evidence decides, and says so.
        let loom = choose_scoped(
            "flash",
            &scoped_cfg(),
            &evidence,
            Some("/home/coding/LOOM"),
            &none,
            None,
            0.5,
        );
        assert_eq!(loom.scope, DecisionScope::Fleet);
        assert!(
            loom.choice.reason.starts_with("fleet:"),
            "{}",
            loom.choice.reason
        );
        let loom_flash = loom
            .considered_workspace
            .iter()
            .find(|e| e.adapter == "flash")
            .map(|e| e.attempts);
        assert_eq!(
            loom_flash,
            Some(4),
            "trailing-slash rows join the workspace"
        );
        assert_eq!(loom.considered_fleet.len(), 3);
    }

    #[test]
    fn nt48_a_poor_workspace_is_signalled_once_per_window_and_never_routed_on() {
        let evidence = Evidence::from_rows(&replay_rows());
        let config = EvidenceRoutingConfig {
            candidates: vec!["flash".into(), "glm".into()],
            ..scoped_cfg()
        };
        let poor = poor_workspaces(&evidence, &config);
        let names: Vec<&str> = poor.iter().map(|p| p.workspace.as_str()).collect();
        assert_eq!(names, vec!["/home/coding/NEEDLE"]);

        // No routing change in the poor workspace: fleet evidence decides.
        let needle = choose_scoped(
            "flash",
            &config,
            &evidence,
            Some("/home/coding/NEEDLE"),
            &HashSet::new(),
            None,
            0.5,
        );
        assert_ne!(needle.scope, DecisionScope::Workspace);
        assert_eq!(needle.choice.adapter, "flash");

        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        assert!(record_poor_workspace(dir.path(), &poor[0], 7, now).unwrap());
        assert!(
            !record_poor_workspace(dir.path(), &poor[0], 7, now).unwrap(),
            "once per window"
        );
        let next_window = now + chrono::Duration::days(7);
        assert!(
            record_poor_workspace(dir.path(), &poor[0], 7, next_window).unwrap(),
            "a new window signals again"
        );

        let off = EvidenceRoutingConfig {
            workspace_poor_threshold: 0.0,
            ..config
        };
        assert!(poor_workspaces(&evidence, &off).is_empty());
    }

    #[test]
    fn nt48_with_workspace_scope_off_the_choice_is_the_fleet_choice_unchanged() {
        let evidence = Evidence::from_rows(&replay_rows());
        let off = EvidenceRoutingConfig {
            workspace_scope: false,
            ..scoped_cfg()
        };
        let none = HashSet::new();
        let scoped = choose_scoped(
            "flash",
            &off,
            &evidence,
            Some("/home/coding/reddit-media-player"),
            &none,
            None,
            0.5,
        );
        let plain = choose("flash", &off, &evidence.fleet, &none, None, 0.5);
        assert_eq!(scoped.choice, plain);
        assert!(scoped.considered_workspace.is_empty());
    }

    #[test]
    fn nt48_freezing_degraded_providers_and_the_default_floor_behave_as_before() {
        let evidence = Evidence::from_rows(&replay_rows());
        let config = scoped_cfg();
        let none = HashSet::new();
        let rmp = Some("/home/coding/reddit-media-player");

        let frozen = choose_scoped(
            "flash",
            &config,
            &evidence,
            rmp,
            &none,
            Some("workspace_degraded"),
            0.5,
        );
        assert_eq!(frozen.choice.adapter, "flash");
        assert_eq!(frozen.scope, DecisionScope::Static);
        assert_eq!(frozen.choice.reason, "static:frozen:workspace_degraded");

        // A degraded provider is never chosen at either scope.
        let degraded: HashSet<String> = ["codex".to_string()].into_iter().collect();
        let c = choose_scoped("flash", &config, &evidence, rmp, &degraded, None, 0.5);
        assert_eq!(c.choice.adapter, "flash");

        // A default with no evidence anywhere keeps its unknown rate.
        let c = choose_scoped("opus", &config, &evidence, rmp, &none, None, 0.5);
        assert_eq!(c.choice.adapter, "opus");
        assert_eq!(c.scope, DecisionScope::Static);
        assert!(
            c.choice
                .reason
                .starts_with("static:insufficient_evidence_for_default"),
            "{}",
            c.choice.reason
        );
    }
}
