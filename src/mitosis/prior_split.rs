//! A prior decomposition remains the plan even after its children close.
//!
//! Title deduplication is insufficient: each retry can describe the same work
//! differently. Check durable parent scope before any new decomposition.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::bead_store::BeadStore;
use crate::types::Bead;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PriorSplit {
    pub children: usize,
    pub unfinished: usize,
}

impl PriorSplit {
    pub fn reason(&self) -> &'static str {
        if self.unfinished > 0 {
            "existing split children remain unfinished; continue the existing plan"
        } else {
            "prior split requires parent acceptance reconciliation, not another decomposition"
        }
    }
}

/// A missing inventory is not proof that this parent has never been split.
pub(crate) async fn inspect(store: &dyn BeadStore, parent: &Bead) -> Result<Option<PriorSplit>> {
    let inventory = tokio::time::timeout(Duration::from_secs(30), store.list_all())
        .await
        .context("timed out checking prior split children")?
        .context("could not check prior split children")?;
    Ok(from_inventory(parent, &inventory))
}

fn from_inventory(parent: &Bead, inventory: &[Bead]) -> Option<PriorSplit> {
    let scope = format!("parent-{}", parent.id);
    let legacy_umbrella = parent.labels.iter().any(|label| label == "umbrella");
    let children: Vec<_> = inventory
        .iter()
        // Historical workers sometimes put the child scope on the parent.
        .filter(|bead| {
            bead.id != parent.id
                && (bead.labels.contains(&scope)
                    || (legacy_umbrella
                        && parent.dependencies.iter().any(|edge| edge.id == bead.id)
                        && bead.labels.iter().any(|label| {
                            matches!(label.as_str(), "split-child" | "mitosis-child")
                        })))
        })
        .collect();
    let provenance = parent
        .labels
        .iter()
        .any(|label| label == super::AUTO_SPLIT_PARENT_LABEL);
    if children.is_empty() && !provenance {
        return None;
    }
    Some(PriorSplit {
        children: children.len(),
        unfinished: children
            .iter()
            .filter(|child| !child.status.is_done())
            .count(),
    })
}
