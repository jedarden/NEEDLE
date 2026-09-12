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
    /// Sum of `estimated_cost_usd` over rows that reported one.
    pub cost_usd: f64,
    /// Rows that reported a cost (denominator for cost per success).
    pub costed: u64,
}

impl AdapterEvidence {
    /// Attempts that were the adapter's to win: everything but
    /// infrastructure failures.
    pub fn judged(&self) -> u64 {
        self.attempts.saturating_sub(self.infrastructure)
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
    let mut candidates: Vec<String> = config.candidates.clone();
    if !candidates.iter().any(|c| c == default) {
        candidates.insert(0, default.to_string());
    }
    let considered: Vec<AdapterEvidence> = candidates
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
        .collect();

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
    let default_rate = evidence
        .get(default)
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

/// Aggregate `attempt.resolved` rows from the telemetry log directory for
/// the last `window_days`, keyed by adapter.
///
/// Reads only files whose date suffix falls in the window and only lines
/// that mention the event, so a 100 GB log directory costs seconds, not
/// minutes. Test and transform logs are skipped.
pub fn evidence_from_logs(log_dir: &Path, window_days: u32) -> HashMap<String, AdapterEvidence> {
    let mut out: HashMap<String, AdapterEvidence> = HashMap::new();
    for row in ledger_rows(log_dir, window_days) {
        let adapter = row
            .get("adapter")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if adapter.is_empty() {
            continue;
        }
        let entry = out
            .entry(adapter.clone())
            .or_insert_with(|| AdapterEvidence {
                adapter,
                ..AdapterEvidence::default()
            });
        entry.attempts += 1;
        match row.get("outcome").and_then(|v| v.as_str()) {
            Some("verified_success") => entry.verified += 1,
            Some("infrastructure_failure") => entry.infrastructure += 1,
            _ => {}
        }
        if let Some(cost) = row.get("estimated_cost_usd").and_then(|v| v.as_f64()) {
            entry.cost_usd += cost;
            entry.costed += 1;
        }
    }
    out
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
    fn infrastructure_failures_do_not_count_against_an_adapter() {
        let e = AdapterEvidence {
            adapter: "a".into(),
            attempts: 30,
            verified: 15,
            infrastructure: 10,
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
}
