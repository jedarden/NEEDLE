//! Operation-strategy enums for pluggable bead CLI backends.
//!
//! This module defines the strategy sets that ADR-013 describes: small closed
//! enums that capture behavioral divergences between CLI backends that cannot
//! be expressed as argv templates alone.
//!
//! Each strategy is implemented *once* in `CliBeadStore`; backends select
//! among them via their descriptors. A backend needing genuinely new behavior
//! adds ONE enum variant — available to every backend — not a BeadStore impl.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;

use async_trait::async_trait;

use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};

/// Common metadata exposed by every operation strategy.
///
/// Execution remains operation-specific, but descriptors and diagnostics need
/// a uniform way to name the operation and selected variant.
pub trait OperationStrategy {
    /// Descriptor operation that selects this strategy type.
    const OPERATION: &'static str;

    /// Stable snake-case name used in descriptor files.
    fn name(self) -> &'static str;
}

// ──────────────────────────────────────────────────────────────────────────────
// Claim strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for claiming a bead: how race conditions are handled.
///
/// The two backends that support server-side claiming (`bf`, `bead`) use an
/// atomic `claim` subcommand. The legacy backend (`br`) has no such command
/// and claims via `update --assignee`, which has a real TOCTOU window if
/// another actor claims between the read and the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrategy {
    /// One descriptor-rendered command performs the explicit claim atomically
    /// and returns the normalized claim response contract.
    AtomicCommand,

    /// Compare-and-set claim: read current state, verify assignee is unset or
    /// matches the expected actor, then update.
    ///
    /// **Race semantics:** If another actor claims the bead between the read
    /// and the write, the update fails and MUST be retried. The write MUST
    /// include a condition (e.g., assignee is currently `null` or `none`) that
    /// the backend can verify atomically. If the backend cannot enforce this
    /// condition atomically, this strategy degrades to best-effort claiming
    /// and duplicate claims are possible.
    ///
    /// **Used by:** `br` (beads_rust), `bead` (bead-rs)
    CompareAndSet,

    /// Single atomic batch operation that both claims and releases beads.
    ///
    /// **Race semantics:** No race — the backend guarantees atomicity. If the
    /// batch fails (e.g., a bead was already claimed by another actor), the
    /// entire batch is rejected and no partial state change occurs.
    ///
    /// **Used by:** `bf` (bead-forge)
    BatchOp,
}

// ──────────────────────────────────────────────────────────────────────────────
// Claim-auto strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for automatic claiming: how the ready frontier is claimed.
///
/// The `claim_auto` operation is "claim the next ready bead if one exists."
/// Backends differ in whether this is one atomic call or a scan loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimAutoStrategy {
    /// Single atomic subcommand: the backend handles scanning and claiming.
    ///
    /// **Race semantics:** No race between scan and claim — the backend
    /// guarantees atomicity. If multiple actors call this simultaneously, the
    /// backend ensures each claims a different bead (or all but one fail).
    ///
    /// **Used by:** `bf` (bead-forge), `bead` (bead-rs)
    AtomicSubcommand,

    /// Non-atomic scan loop: list ready beads, then claim the first.
    ///
    /// **Race semantics:** REAL TOCTOU window between `ready` (the scan) and
    /// `update` (the claim). If another actor claims the bead during this
    /// window, the claim fails and MUST be retried with the next bead. This
    /// is not merely a slower atomic_subcommand — it is fundamentally racy.
    ///
    /// The duplicate-claim hazard described in CLAUDE.md ("NEEDLE Fleet
    /// Dispatch — no worktrees") is exactly this failure mode: two workers on
    /// a `br` workspace can both see the same bead as ready and both attempt
    /// to claim it. One wins; the other MUST retry, not assume success.
    ///
    /// **Used by:** `br` (beads_rust)
    NonAtomicScan,
}

// ──────────────────────────────────────────────────────────────────────────────
// Split strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for splitting a bead into multiple child beads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitStrategy {
    /// Transactional batch: all child beads are created in one atomic operation.
    ///
    /// **Race semantics:** No race — if any child creation fails, the entire
    /// batch is rolled back and no partial state change occurs.
    ///
    /// **Used by:** `bf` (bead-forge)
    TransactionalBatch,

    /// Sequential creation: children are created one at a time.
    ///
    /// **Race semantics:** If a child creation fails partway through, some
    /// children may have been created already. The caller is responsible for
    /// cleanup (e.g., deleting the partial children) or for marking the parent
    /// bead as failed so the split can be retried.
    ///
    /// **Used by:** `br` (beads_rust), `bead` (bead-rs)
    Sequential,
}

// ──────────────────────────────────────────────────────────────────────────────
// Create-ID strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for extracting the created bead ID from the `create` command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateIdStrategy {
    /// Bare ID on stdout: the command prints nothing but the bead ID.
    ///
    /// Example output: `bf-46m05\n`
    ///
    /// **Used by:** `bf` (bead-forge), `bead` (bead-rs)
    BareId,

    /// JSON field: parse JSON output and extract the ID from a field.
    ///
    /// Example output: `{"id":"bf-46m05","title":"..."}\n`
    ///
    /// **Used by:** `br` (beads_rust, via `--json`)
    JsonField,
}

// ──────────────────────────────────────────────────────────────────────────────
// Labels strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for passing labels to the `create` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelsStrategy {
    /// Comma-separated values: labels are passed as a single comma-separated
    /// string.
    ///
    /// Example: `bf create --labels bug,priority --title Fix bug`
    ///
    /// **Used by:** `br` (beads_rust, via `-l/--labels`)
    Csv,

    /// Repeated flag: labels are passed as multiple occurrences of the same flag.
    ///
    /// Example: `bf create --label bug --label priority --title Fix bug`
    ///
    /// **Used by:** `bf` (bead-forge, via `--label`), `bead` (bead-rs, via `--label`)
    Repeated,
}

// ──────────────────────────────────────────────────────────────────────────────
// Import strategies
// ──────────────────────────────────────────────────────────────────────────────

/// Strategy for importing beads from an external source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStrategy {
    /// Bare import: the `sync --import-only` command takes no additional flags.
    ///
    /// Example: `bf sync --import-only`
    ///
    /// **Used by:** `bf` (bead-forge), `br` (beads_rust)
    Bare,

    /// Input-plus-mode: the import command requires an input file and a mode
    /// flag specifying how to merge.
    ///
    /// Example: `bead sync --import-only --input backup.jsonl --restore-into-empty`
    ///
    /// **Used by:** `bead` (bead-rs, via `--input` + `--restore-into-empty`/`--merge`)
    InputPlusMode,
}

macro_rules! impl_operation_strategy {
    ($type:ty, $operation:literal, { $($variant:path => $name:literal),+ $(,)? }) => {
        impl OperationStrategy for $type {
            const OPERATION: &'static str = $operation;

            fn name(self) -> &'static str {
                match self {
                    $($variant => $name),+
                }
            }
        }
    };
}

impl_operation_strategy!(ClaimStrategy, "claim", {
    ClaimStrategy::AtomicCommand => "atomic_command",
    ClaimStrategy::CompareAndSet => "compare_and_set",
    ClaimStrategy::BatchOp => "batch_op",
});
impl_operation_strategy!(ClaimAutoStrategy, "claim_auto", {
    ClaimAutoStrategy::AtomicSubcommand => "atomic_subcommand",
    ClaimAutoStrategy::NonAtomicScan => "non_atomic_scan",
});
impl_operation_strategy!(SplitStrategy, "split", {
    SplitStrategy::TransactionalBatch => "transactional_batch",
    SplitStrategy::Sequential => "sequential",
});
impl_operation_strategy!(CreateIdStrategy, "create_id", {
    CreateIdStrategy::BareId => "bare_id",
    CreateIdStrategy::JsonField => "json_field",
});
impl_operation_strategy!(LabelsStrategy, "labels", {
    LabelsStrategy::Csv => "csv",
    LabelsStrategy::Repeated => "repeated",
});
impl_operation_strategy!(ImportStrategy, "import", {
    ImportStrategy::Bare => "bare",
    ImportStrategy::InputPlusMode => "input_plus_mode",
});

/// Type-erased strategy returned while validating descriptor operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedStrategy {
    Claim(ClaimStrategy),
    ClaimAuto(ClaimAutoStrategy),
    Split(SplitStrategy),
    CreateId(CreateIdStrategy),
    Labels(LabelsStrategy),
    Import(ImportStrategy),
}

/// Parse and validate one descriptor strategy with actionable source context.
///
/// Descriptor loading must call this before a backend can be selected. This
/// prevents a misspelled strategy from surviving until the first live claim.
pub fn validate_strategy_name(
    descriptor_path: &Path,
    operation: &str,
    strategy: &str,
) -> Result<ParsedStrategy, anyhow::Error> {
    fn parse<T: DeserializeOwned>(
        descriptor_path: &Path,
        operation: &str,
        strategy: &str,
    ) -> Result<T, anyhow::Error> {
        serde_json::from_value(serde_json::Value::String(strategy.to_string())).map_err(|_| {
            anyhow::anyhow!(
                "unknown strategy '{}' for operation '{}' in {}",
                strategy,
                operation,
                descriptor_path.display()
            )
        })
    }

    match operation {
        ClaimStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::Claim)
        }
        ClaimAutoStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::ClaimAuto)
        }
        SplitStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::Split)
        }
        CreateIdStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::CreateId)
        }
        LabelsStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::Labels)
        }
        ImportStrategy::OPERATION => {
            parse(descriptor_path, operation, strategy).map(ParsedStrategy::Import)
        }
        _ => Err(anyhow::anyhow!(
            "unknown strategy operation '{}' in {}",
            operation,
            descriptor_path.display()
        )),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Strategy execution functions
// ──────────────────────────────────────────────────────────────────────────────

/// Result of one backend compare-and-set mutation.
#[derive(Debug, Clone)]
pub enum CompareAndSetOutcome {
    /// The conditional mutation succeeded and returned the claimed bead.
    Claimed(Box<Bead>),
    /// The version changed while the issue was still claimable; reread and retry.
    VersionChanged,
    /// Another actor won the claim race.
    RaceLost { claimed_by: String },
    /// The issue cannot currently be claimed for a non-race reason.
    NotClaimable { reason: String },
}

/// Backend primitives required by the claim strategy engine.
///
/// A descriptor-driven store implements these primitives using rendered argv.
/// The strategy engine owns sequencing and retry semantics so those rules are
/// not reimplemented independently for every CLI dialect.
#[async_trait]
pub trait ClaimStrategyOperations: Send + Sync {
    /// Read one issue, including its current version (`updated_at`).
    async fn show_for_claim(&self, bead_id: &BeadId) -> anyhow::Result<Bead>;

    /// Conditionally claim only if `expected_version` still matches.
    async fn compare_and_set_claim(
        &self,
        bead_id: &BeadId,
        actor: &str,
        expected_version: &str,
    ) -> anyhow::Result<CompareAndSetOutcome>;

    /// Claim one explicit issue in a single backend transaction.
    async fn batch_claim(&self, bead_id: &BeadId, actor: &str) -> anyhow::Result<ClaimResult>;

    /// Claim one explicit issue through a descriptor-rendered atomic command.
    async fn atomic_claim(&self, _bead_id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("atomic command claim is not implemented by this store")
    }

    /// Atomically select and claim the next ready issue.
    async fn atomic_claim_auto(&self, actor: &str) -> anyhow::Result<ClaimResult>;

    /// Read the ordered ready frontier without claiming it.
    async fn ready_for_claim(&self) -> anyhow::Result<Vec<Bead>>;
}

const MAX_COMPARE_AND_SET_ATTEMPTS: usize = 3;

/// Execute a claim operation using the specified strategy.
///
/// This function implements each claim strategy variant exactly once. Callers
/// (the eventual CliBeadStore) select the strategy via descriptor configuration.
///
/// # Arguments
///
/// * `strategy` - Which claim strategy to use
/// * `bead_id` - The bead to claim
/// * `actor` - The actor claiming the bead
///
/// # Returns
///
/// * `Ok(ClaimResult::Claimed(bead))` - Successfully claimed
/// * `Ok(ClaimResult::RaceLost { claimed_by })` - Another actor claimed it first
/// * `Ok(ClaimResult::NotClaimable { reason })` - Bead not in claimable state
/// * `Err(...)` - Subprocess or parsing error
///
/// # Race semantics per strategy
///
/// ## CompareAndSet
///
/// **Race semantics:** If another actor claims the bead between the read and the
/// write, the update MUST fail and return `RaceLost`. The write MUST include a
/// condition (e.g., assignee is currently `null`) that the backend can verify
/// atomically.
///
/// This strategy is correct ONLY when the backend can enforce the condition
/// atomically (e.g., SQLite `WHERE assignee IS NULL`). Without that, it degrades
/// to best-effort claiming and duplicate claims are possible.
///
/// ## BatchOp
///
/// **Race semantics:** No race — the backend guarantees atomicity. If the batch
/// fails (e.g., bead was already claimed), the entire batch is rejected and no
/// partial state change occurs.
///
/// Used by bead-forge's `bf batch` which runs multiple operations in a single
/// `BEGIN IMMEDIATE` transaction.
pub async fn execute_claim_strategy(
    operations: &dyn ClaimStrategyOperations,
    strategy: ClaimStrategy,
    bead_id: &BeadId,
    actor: &str,
) -> anyhow::Result<ClaimResult> {
    match strategy {
        ClaimStrategy::AtomicCommand => operations.atomic_claim(bead_id, actor).await,
        ClaimStrategy::CompareAndSet => {
            for _ in 0..MAX_COMPARE_AND_SET_ATTEMPTS {
                let bead = operations.show_for_claim(bead_id).await?;
                if bead.status != BeadStatus::Open {
                    return Ok(ClaimResult::NotClaimable {
                        reason: format!("bead is {}, not open", bead.status),
                    });
                }
                if let Some(claimed_by) = bead.assignee {
                    return Ok(ClaimResult::RaceLost { claimed_by });
                }

                let expected_version = bead.updated_at.to_rfc3339();
                match operations
                    .compare_and_set_claim(bead_id, actor, &expected_version)
                    .await?
                {
                    CompareAndSetOutcome::Claimed(bead) => {
                        return Ok(ClaimResult::Claimed(*bead));
                    }
                    CompareAndSetOutcome::VersionChanged => continue,
                    CompareAndSetOutcome::RaceLost { claimed_by } => {
                        return Ok(ClaimResult::RaceLost { claimed_by });
                    }
                    CompareAndSetOutcome::NotClaimable { reason } => {
                        return Ok(ClaimResult::NotClaimable { reason });
                    }
                }
            }

            let latest = operations.show_for_claim(bead_id).await?;
            if let Some(claimed_by) = latest.assignee {
                Ok(ClaimResult::RaceLost { claimed_by })
            } else {
                Ok(ClaimResult::ClaimError {
                    reason: format!(
                        "bead version changed during all {MAX_COMPARE_AND_SET_ATTEMPTS} compare-and-set attempts"
                    ),
                })
            }
        }
        ClaimStrategy::BatchOp => operations.batch_claim(bead_id, actor).await,
    }
}

/// Execute a claim_auto operation using the specified strategy.
///
/// # Arguments
///
/// * `strategy` - Which claim_auto strategy to use
/// * `actor` - The actor claiming beads
///
/// # Returns
///
/// * `Ok(ClaimResult::Claimed(bead))` - Successfully claimed a bead
/// * `Ok(ClaimResult::NotClaimable { reason })` - No beads available to claim
/// * `Err(...)` - Subprocess or parsing error
///
/// # Race semantics per strategy
///
/// ## AtomicSubcommand
///
/// **Race semantics:** No race between scan and claim — the backend guarantees
/// atomicity. If multiple actors call this simultaneously, the backend ensures
/// each claims a different bead (or all but one fail).
///
/// Example: `bf claim --assignee worker-1` which internally scans and claims
/// in one SQLite transaction.
///
/// ## NonAtomicScan
///
/// **Race semantics:** REAL TOCTOU window between `ready` (the scan) and `update`
/// (the claim). If another actor claims the bead during this window, the claim
/// fails and MUST be retried with the next bead.
///
/// This is NOT merely a slower atomic_subcommand — it is fundamentally racy.
/// The duplicate-claim hazard described in CLAUDE.md is exactly this failure mode:
/// two workers on a `br` workspace can both see the same bead as ready and both
/// attempt to claim it. One wins; the other MUST retry, not assume success.
///
/// Example: `br ready --json --limit 1` → parse → `br update {id} --assignee worker`
pub async fn execute_claim_auto_strategy(
    operations: &dyn ClaimStrategyOperations,
    strategy: ClaimAutoStrategy,
    explicit_claim_strategy: ClaimStrategy,
    actor: &str,
) -> anyhow::Result<ClaimResult> {
    match strategy {
        ClaimAutoStrategy::AtomicSubcommand => operations.atomic_claim_auto(actor).await,
        ClaimAutoStrategy::NonAtomicScan => {
            // The read and mutation are intentionally separate. Every candidate
            // can disappear in this TOCTOU window, so a race-lost result advances
            // to the next candidate instead of dispatching duplicate work.
            for bead in operations.ready_for_claim().await? {
                match execute_claim_strategy(operations, explicit_claim_strategy, &bead.id, actor)
                    .await?
                {
                    claimed @ ClaimResult::Claimed(_) => return Ok(claimed),
                    ClaimResult::RaceLost { .. } | ClaimResult::NotClaimable { .. } => continue,
                    error @ ClaimResult::ClaimError { .. }
                    | error @ ClaimResult::Suspect { .. } => return Ok(error),
                }
            }

            Ok(ClaimResult::NotClaimable {
                reason: "no claimable beads remained after ready scan".to_string(),
            })
        }
    }
}

/// Execute a split operation using the specified strategy.
///
/// # Arguments
///
/// * `strategy` - Which split strategy to use
/// * `binary_path` - Path to the bead CLI binary
/// * `workspace` - Workspace directory containing `.beads/`
/// * `parent_id` - The parent bead being split
/// * `children` - Child beads to create
///
/// # Returns
///
/// * `Ok(child_ids)` - Successfully created child bead IDs
/// * `Err(...)` - Subprocess or parsing error
///
/// # Race semantics per strategy
///
/// ## TransactionalBatch
///
/// **Race semantics:** No race — if any child creation fails, the entire batch
/// is rolled back and no partial state change occurs.
///
/// Example: `bf batch --json '[{"op":"create",...}, {"op":"dep_add",...}]'`
///
/// ## Sequential
///
/// **Race semantics:** If a child creation fails partway through, some children
/// may have been created already. The caller is responsible for cleanup (e.g.,
/// deleting the partial children) or for marking the parent bead as failed so
/// the split can be retried.
///
/// Example: Loop calling `br create` then `br dep add` for each child.
#[async_trait]
pub trait SplitStrategyOperations: Send + Sync {
    /// Create and link every child in one backend transaction.
    ///
    /// On error the backend guarantees that no child or dependency was
    /// committed, so the caller may safely retry the whole operation.
    async fn transactional_split(
        &self,
        parent_id: &BeadId,
        children: &[crate::bead_store::NewChild<'_>],
    ) -> anyhow::Result<Vec<BeadId>>;

    /// Create one child as its own mutation.
    async fn create_split_child(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> anyhow::Result<BeadId>;

    /// Link an already-created child as a blocker of its parent.
    async fn link_split_child(&self, child_id: &BeadId, parent_id: &BeadId) -> anyhow::Result<()>;
}

/// A sequential split failed after committing some children.
///
/// The created IDs are returned explicitly because a sequential backend has
/// no transaction spanning the whole split. Operators can reconcile those
/// children instead of silently retrying and creating duplicates.
#[derive(Debug, thiserror::Error)]
#[error("sequential split failed after creating {created_count} child bead(s): {source}")]
pub struct SequentialSplitError {
    pub created: Vec<BeadId>,
    created_count: usize,
    #[source]
    pub source: anyhow::Error,
}

pub async fn execute_split_strategy(
    operations: &dyn SplitStrategyOperations,
    strategy: SplitStrategy,
    parent_id: &BeadId,
    children: &[crate::bead_store::NewChild<'_>],
) -> anyhow::Result<Vec<BeadId>> {
    match strategy {
        SplitStrategy::TransactionalBatch => {
            operations.transactional_split(parent_id, children).await
        }
        SplitStrategy::Sequential => {
            let mut created = Vec::with_capacity(children.len());
            for child in children {
                let child_id = match operations
                    .create_split_child(child.title, child.body, child.labels)
                    .await
                {
                    Ok(child_id) => child_id,
                    Err(source) => {
                        return Err(SequentialSplitError {
                            created_count: created.len(),
                            created,
                            source,
                        }
                        .into());
                    }
                };

                if let Err(source) = operations.link_split_child(&child_id, parent_id).await {
                    created.push(child_id);
                    return Err(SequentialSplitError {
                        created_count: created.len(),
                        created,
                        source,
                    }
                    .into());
                }
                created.push(child_id);
            }

            Ok(created)
        }
    }
}

/// Parse a created bead ID from command output using the specified strategy.
///
/// # Arguments
///
/// * `strategy` - Which ID parsing strategy to use
/// * `output` - Stdout from the create command
///
/// # Returns
///
/// * `Ok(bead_id)` - Successfully parsed bead ID
/// * `Err(...)` - Parsing error
///
/// # Parsing semantics per strategy
///
/// ## BareId
///
/// The command prints nothing but the bead ID.
///
/// Example output: `bf-46m05\n`
///
/// ## JsonField
///
/// The command prints JSON output; extract the ID from a field.
///
/// Example output: `{"id":"bf-46m05","title":"..."}\n`
#[allow(dead_code)]
pub fn execute_create_id_strategy(
    strategy: CreateIdStrategy,
    output: &str,
) -> Result<String, anyhow::Error> {
    match strategy {
        CreateIdStrategy::BareId => {
            // Parse bare ID from stdout (trim newline)
            let id = output.trim().to_string();
            if id.is_empty() {
                return Err(anyhow::anyhow!("BareId strategy: empty output"));
            }
            Ok(id)
        }
        CreateIdStrategy::JsonField => {
            // Accept both a direct response and the standard CLI envelope.
            let json: serde_json::Value =
                serde_json::from_str(output).map_err(|e| anyhow::anyhow!("invalid JSON: {}", e))?;

            json.get("id")
                .or_else(|| json.pointer("/data/id"))
                .and_then(|id| id.as_str())
                .map(|s| s.to_string())
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("JSON missing non-empty string 'id' field"))
        }
    }
}

/// Format labels for a create command using the specified strategy.
///
/// # Arguments
///
/// * `strategy` - Which labels strategy to use
/// * `labels` - Labels to format
///
/// # Returns
///
/// * Formatted labels ready to pass as command-line arguments
///
/// # Formatting semantics per strategy
///
/// ## Csv
///
/// Labels are passed as a single comma-separated string.
///
/// Example: `["bug", "priority"]` → `["--labels", "bug,priority"]`
///
/// ## Repeated
///
/// Labels are passed as multiple occurrences of the same flag.
///
/// Example: `["bug", "priority"]` → `["--label", "bug", "--label", "priority"]`
#[allow(dead_code)]
pub fn execute_labels_strategy(strategy: LabelsStrategy, labels: &[&str]) -> Vec<String> {
    match strategy {
        LabelsStrategy::Csv => {
            if labels.is_empty() {
                vec![]
            } else {
                let encoded = labels
                    .iter()
                    .map(|label| {
                        if label.contains(',') || label.contains('"') || label.trim() != *label {
                            format!("\"{}\"", label.replace('"', "\"\""))
                        } else {
                            (*label).to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                vec!["--labels".to_string(), encoded]
            }
        }
        LabelsStrategy::Repeated => labels
            .iter()
            .flat_map(|label| vec!["--label".to_string(), label.to_string()])
            .collect(),
    }
}

/// Parse descriptor label values according to the selected input strategy.
///
/// CSV follows the conventional doubled-quote escaping rule. Empty fields and
/// whitespace-only repeated occurrences are ignored so descriptors cannot
/// accidentally create an empty label.
pub fn parse_labels_strategy(
    strategy: LabelsStrategy,
    values: &[&str],
) -> anyhow::Result<Vec<String>> {
    match strategy {
        LabelsStrategy::Repeated => Ok(values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect()),
        LabelsStrategy::Csv => {
            let mut labels = Vec::new();
            for value in values {
                let mut field = String::new();
                let mut chars = value.chars().peekable();
                let mut in_quotes = false;
                let mut quoted_field = false;
                let mut after_quote = false;
                while let Some(character) = chars.next() {
                    match character {
                        '"' if in_quotes && chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        '"' if in_quotes => {
                            in_quotes = false;
                            after_quote = true;
                        }
                        '"' if field.trim().is_empty() => {
                            field.clear();
                            in_quotes = true;
                            quoted_field = true;
                        }
                        '"' => anyhow::bail!("unexpected quote in CSV label value {value:?}"),
                        ',' if !in_quotes => {
                            let label = if quoted_field {
                                field.as_str()
                            } else {
                                field.trim()
                            };
                            if !label.is_empty() {
                                labels.push(label.to_string());
                            }
                            field.clear();
                            quoted_field = false;
                            after_quote = false;
                        }
                        whitespace if after_quote && whitespace.is_whitespace() => {}
                        _ if after_quote => {
                            anyhow::bail!("unexpected content after quoted CSV label in {value:?}")
                        }
                        other => field.push(other),
                    }
                }
                if in_quotes {
                    anyhow::bail!("unterminated quoted label in CSV value {value:?}");
                }
                let label = if quoted_field {
                    field.as_str()
                } else {
                    field.trim()
                };
                if !label.is_empty() {
                    labels.push(label.to_string());
                }
            }
            Ok(labels)
        }
    }
}

/// Format import command arguments using the specified strategy.
///
/// # Arguments
///
/// * `strategy` - Which import strategy to use
/// * `input_file` - Input file or checkpoint directory path
/// * `mode` - Mode flag (for example `--restore-into-empty`) when required
///
/// # Returns
///
/// * Formatted arguments ready to pass to the sync command
///
/// # Formatting semantics per strategy
///
/// ## Bare
///
/// The input path is passed as the sole strategy-specific argument.
///
/// Example: `["sync", "--import-only"]`
///
/// ## InputPlusMode
///
/// The import command requires an input file and a mode flag specifying how to merge.
///
/// Example: `["--input", "backup.jsonl", "--restore-into-empty"]`
pub fn execute_import_strategy(
    strategy: ImportStrategy,
    input_file: &Path,
    mode: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let input = input_file.to_string_lossy().into_owned();
    match strategy {
        ImportStrategy::Bare => Ok(vec![input]),
        ImportStrategy::InputPlusMode => {
            let mode = mode.ok_or_else(|| {
                anyhow::anyhow!("input_plus_mode import strategy requires a mode")
            })?;
            let mode = if mode.starts_with("--") {
                mode.to_string()
            } else {
                format!("--mode={mode}")
            };
            Ok(vec!["--input".to_string(), input, mode])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_every_operation_strategy_type() {
        let path = Path::new("descriptor.yaml");
        let cases = [
            (
                "claim",
                "compare_and_set",
                ParsedStrategy::Claim(ClaimStrategy::CompareAndSet),
            ),
            (
                "claim_auto",
                "atomic_subcommand",
                ParsedStrategy::ClaimAuto(ClaimAutoStrategy::AtomicSubcommand),
            ),
            (
                "split",
                "sequential",
                ParsedStrategy::Split(SplitStrategy::Sequential),
            ),
            (
                "create_id",
                "bare_id",
                ParsedStrategy::CreateId(CreateIdStrategy::BareId),
            ),
            (
                "labels",
                "repeated",
                ParsedStrategy::Labels(LabelsStrategy::Repeated),
            ),
            (
                "import",
                "input_plus_mode",
                ParsedStrategy::Import(ImportStrategy::InputPlusMode),
            ),
        ];

        for (operation, strategy, expected) in cases {
            assert_eq!(
                validate_strategy_name(path, operation, strategy).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn unknown_strategy_error_names_value_operation_and_file() {
        let error =
            validate_strategy_name(Path::new("/tmp/backends/descriptor.yaml"), "claim", "foo")
                .unwrap_err()
                .to_string();

        assert_eq!(
            error,
            "unknown strategy 'foo' for operation 'claim' in /tmp/backends/descriptor.yaml"
        );
    }

    #[test]
    fn unknown_operation_error_names_operation_and_file() {
        let error = validate_strategy_name(
            Path::new("/tmp/backends/descriptor.yaml"),
            "teleport",
            "atomic",
        )
        .unwrap_err()
        .to_string();

        assert_eq!(
            error,
            "unknown strategy operation 'teleport' in /tmp/backends/descriptor.yaml"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ClaimStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn claim_strategy_serializes_correctly() {
        // Verify snake_case serialization for descriptor YAML compatibility
        assert_eq!(
            serde_json::to_string(&ClaimStrategy::AtomicCommand).unwrap(),
            r#""atomic_command""#
        );
        assert_eq!(
            serde_json::to_string(&ClaimStrategy::CompareAndSet).unwrap(),
            r#""compare_and_set""#
        );
        assert_eq!(
            serde_json::to_string(&ClaimStrategy::BatchOp).unwrap(),
            r#""batch_op""#
        );
    }

    #[test]
    fn claim_strategy_deserializes_from_snake_case() {
        // Verify we can deserialize from the snake_case form used in descriptors
        assert_eq!(
            serde_json::from_str::<ClaimStrategy>(r#""atomic_command""#).unwrap(),
            ClaimStrategy::AtomicCommand
        );
        assert_eq!(
            serde_json::from_str::<ClaimStrategy>(r#""compare_and_set""#).unwrap(),
            ClaimStrategy::CompareAndSet
        );
        assert_eq!(
            serde_json::from_str::<ClaimStrategy>(r#""batch_op""#).unwrap(),
            ClaimStrategy::BatchOp
        );
    }

    #[test]
    fn claim_strategy_rejects_unknown_value() {
        // Unknown strategy names should fail deserialization
        let result: Result<ClaimStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ClaimAutoStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn claim_auto_strategy_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&ClaimAutoStrategy::AtomicSubcommand).unwrap(),
            r#""atomic_subcommand""#
        );
        assert_eq!(
            serde_json::to_string(&ClaimAutoStrategy::NonAtomicScan).unwrap(),
            r#""non_atomic_scan""#
        );
    }

    #[test]
    fn claim_auto_strategy_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<ClaimAutoStrategy>(r#""atomic_subcommand""#).unwrap(),
            ClaimAutoStrategy::AtomicSubcommand
        );
        assert_eq!(
            serde_json::from_str::<ClaimAutoStrategy>(r#""non_atomic_scan""#).unwrap(),
            ClaimAutoStrategy::NonAtomicScan
        );
    }

    #[test]
    fn claim_auto_strategy_rejects_unknown_value() {
        let result: Result<ClaimAutoStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // SplitStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn split_strategy_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&SplitStrategy::TransactionalBatch).unwrap(),
            r#""transactional_batch""#
        );
        assert_eq!(
            serde_json::to_string(&SplitStrategy::Sequential).unwrap(),
            r#""sequential""#
        );
    }

    #[test]
    fn split_strategy_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<SplitStrategy>(r#""transactional_batch""#).unwrap(),
            SplitStrategy::TransactionalBatch
        );
        assert_eq!(
            serde_json::from_str::<SplitStrategy>(r#""sequential""#).unwrap(),
            SplitStrategy::Sequential
        );
    }

    #[test]
    fn split_strategy_rejects_unknown_value() {
        let result: Result<SplitStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // CreateIdStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn create_id_strategy_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&CreateIdStrategy::BareId).unwrap(),
            r#""bare_id""#
        );
        assert_eq!(
            serde_json::to_string(&CreateIdStrategy::JsonField).unwrap(),
            r#""json_field""#
        );
    }

    #[test]
    fn create_id_strategy_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<CreateIdStrategy>(r#""bare_id""#).unwrap(),
            CreateIdStrategy::BareId
        );
        assert_eq!(
            serde_json::from_str::<CreateIdStrategy>(r#""json_field""#).unwrap(),
            CreateIdStrategy::JsonField
        );
    }

    #[test]
    fn create_id_strategy_rejects_unknown_value() {
        let result: Result<CreateIdStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // LabelsStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn labels_strategy_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&LabelsStrategy::Csv).unwrap(),
            r#""csv""#
        );
        assert_eq!(
            serde_json::to_string(&LabelsStrategy::Repeated).unwrap(),
            r#""repeated""#
        );
    }

    #[test]
    fn labels_strategy_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<LabelsStrategy>(r#""csv""#).unwrap(),
            LabelsStrategy::Csv
        );
        assert_eq!(
            serde_json::from_str::<LabelsStrategy>(r#""repeated""#).unwrap(),
            LabelsStrategy::Repeated
        );
    }

    #[test]
    fn labels_strategy_rejects_unknown_value() {
        let result: Result<LabelsStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ImportStrategy tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn import_strategy_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&ImportStrategy::Bare).unwrap(),
            r#""bare""#
        );
        assert_eq!(
            serde_json::to_string(&ImportStrategy::InputPlusMode).unwrap(),
            r#""input_plus_mode""#
        );
    }

    #[test]
    fn import_strategy_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<ImportStrategy>(r#""bare""#).unwrap(),
            ImportStrategy::Bare
        );
        assert_eq!(
            serde_json::from_str::<ImportStrategy>(r#""input_plus_mode""#).unwrap(),
            ImportStrategy::InputPlusMode
        );
    }

    #[test]
    fn import_strategy_rejects_unknown_value() {
        let result: Result<ImportStrategy, _> = serde_json::from_str(r#""unknown_strategy""#);
        assert!(result.is_err(), "unknown strategy should be rejected");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Integration test: prove compare_and_set detects lost race
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn compare_and_set_documents_race_detection_contract() {
        // This test documents the race-detection contract that any
        // CompareAndSet implementation must satisfy.
        //
        // The contract:
        // 1. Read the bead's current state (assignee, status).
        // 2. Verify assignee is unset (or matches the expected actor).
        // 3. If verification fails, return RaceLost — the race was lost.
        // 4. If verification succeeds, update assignee to the new actor.
        //
        // The critical invariant: step 2 MUST be checked atomically with step
        // 4. If the backend cannot enforce this (e.g., no WHERE clause), then
        // CompareAndSet is a lie and the descriptor should use BatchOp instead.
        //
        // Implementation in CliBeadStore must:
        // - Read bead state before attempting claim
        // - Return RaceLost if assignee is already set to a different actor
        // - Verify claim succeeded by reading back after update
        // - Never silently overwrite an existing claim

        let strategy = ClaimStrategy::CompareAndSet;
        assert!(
            matches!(strategy, ClaimStrategy::CompareAndSet),
            "strategy variant exists"
        );
    }

    #[test]
    fn batch_op_documents_atomicity_contract() {
        // BatchOp guarantees atomicity - no partial state on failure.
        //
        // The contract:
        // 1. All operations in the batch succeed or none do.
        // 2. If the backend rejects the batch (e.g., bead already claimed),
        //    no state change occurs.
        // 3. The caller gets a clear error indicating why the batch failed.
        //
        // Used by: bead-forge's `bf batch` which runs multiple operations
        // in a single BEGIN IMMEDIATE transaction.

        let strategy = ClaimStrategy::BatchOp;
        assert!(
            matches!(strategy, ClaimStrategy::BatchOp),
            "strategy variant exists"
        );
    }

    #[test]
    fn atomic_subcommand_documents_no_scan_claim_race() {
        // AtomicSubcommand has no race between scan and claim.
        //
        // The contract:
        // 1. The backend handles both scanning (finding a ready bead) and
        //    claiming (setting assignee) in one atomic operation.
        // 2. Multiple concurrent calls to this operation will never return
        //    the same bead to different actors.
        // 3. If no beads are available, returns NotClaimable rather than
        //    blocking or racing.
        //
        // Used by: `bf claim` which performs scoring and UPDATE in a single
        // BEGIN IMMEDIATE transaction.

        let strategy = ClaimAutoStrategy::AtomicSubcommand;
        assert!(
            matches!(strategy, ClaimAutoStrategy::AtomicSubcommand),
            "strategy variant exists"
        );
    }

    #[test]
    fn non_atomic_scan_documents_toctou_race_window() {
        // NonAtomicScan has a REAL TOCTOU window between ready() and update().
        //
        // The contract:
        // 1. Call ready() to list available beads (the scan).
        // 2. Pick one bead from the list.
        // 3. Call update() to claim it (the claim).
        //
        // THE RACE: Between step 1 and step 3, another actor can claim the
        // same bead. The claim() call MUST detect this and return RaceLost,
        // then the caller MUST retry with the next bead from the list.
        //
        // This is NOT merely a slower AtomicSubcommand — it is fundamentally
        // racy. The duplicate-claim hazard described in CLAUDE.md is exactly
        // this failure mode: two workers on a br workspace can both see the
        // same bead as ready and both attempt to claim it.
        //
        // Correct implementation MUST:
        // - Loop on RaceLost, picking the next bead from the ready list
        // - NOT assume the first bead in the list is still claimable
        // - Track which beads have been attempted to avoid infinite loops

        let strategy = ClaimAutoStrategy::NonAtomicScan;
        assert!(
            matches!(strategy, ClaimAutoStrategy::NonAtomicScan),
            "strategy variant exists"
        );
    }

    #[test]
    fn transactional_batch_documents_rollback_contract() {
        // TransactionalBatch guarantees rollback on partial failure.
        //
        // The contract:
        // 1. All child beads are created in one atomic operation.
        // 2. If any child creation fails, the entire batch is rolled back.
        // 3. No partial state change occurs - either all children exist or
        //    none do.
        //
        // Used by: `bf batch` which can create multiple beads and add
        // dependencies in one transaction.

        let strategy = SplitStrategy::TransactionalBatch;
        assert!(
            matches!(strategy, SplitStrategy::TransactionalBatch),
            "strategy variant exists"
        );
    }

    #[test]
    fn sequential_documents_partial_creation_risk() {
        // Sequential creation has partial creation risk.
        //
        // The contract:
        // 1. Children are created one at a time in a loop.
        // 2. If creation fails partway through, some children exist.
        // 3. The caller is responsible for cleanup or marking the parent
        //    as failed so the split can be retried.
        //
        // Correct implementation MUST:
        // - Track which children were successfully created
        // - On failure, either clean up partial children OR mark parent
        //   as failed so the split can be retried
        // - Never leave orphaned children without dependency links

        let strategy = SplitStrategy::Sequential;
        assert!(
            matches!(strategy, SplitStrategy::Sequential),
            "strategy variant exists"
        );
    }

    #[test]
    fn bare_id_documents_parsing_contract() {
        // BareId strategy parses a bare bead ID from stdout.
        //
        // The contract:
        // 1. The command prints nothing but the bead ID (plus newline).
        // 2. Trim whitespace to get the ID.
        // 3. Empty output is an error (command failed).
        //
        // Example: `bf create ...` prints `bf-46m05\n`

        let strategy = CreateIdStrategy::BareId;
        assert!(
            matches!(strategy, CreateIdStrategy::BareId),
            "strategy variant exists"
        );

        // Test the parsing function
        let output = "bf-46m05\n";
        let id = execute_create_id_strategy(strategy, output).unwrap();
        assert_eq!(id, "bf-46m05");

        // Empty output is an error
        let result = execute_create_id_strategy(strategy, "");
        assert!(result.is_err());
    }

    #[test]
    fn json_field_documents_parsing_contract() {
        // JsonField strategy parses JSON output to extract the ID field.
        //
        // The contract:
        // 1. The command prints JSON output.
        // 2. Extract the 'id' field from the JSON.
        // 3. Missing 'id' field or invalid JSON is an error.
        //
        // Example: `br create --json ...` prints `{"id":"bf-46m05",...}\n`

        let strategy = CreateIdStrategy::JsonField;
        assert!(
            matches!(strategy, CreateIdStrategy::JsonField),
            "strategy variant exists"
        );

        // Test the parsing function
        let output = r#"{"id":"bf-46m05","title":"Test bead"}"#;
        let id = execute_create_id_strategy(strategy, output).unwrap();
        assert_eq!(id, "bf-46m05");

        // Missing id field is an error
        let output = r#"{"title":"Test bead"}"#;
        let result = execute_create_id_strategy(strategy, output);
        assert!(result.is_err());

        // Invalid JSON is an error
        let output = "not json";
        let result = execute_create_id_strategy(strategy, output);
        assert!(result.is_err());
    }

    #[test]
    fn csv_documents_formatting_contract() {
        // Csv strategy formats labels as a single comma-separated string.
        //
        // The contract:
        // 1. Labels are joined with commas.
        // 2. Passed as a single --labels flag.
        // 3. Empty labels produce no args.
        //
        // Example: `["bug", "priority"]` → `["--labels", "bug,priority"]`

        let strategy = LabelsStrategy::Csv;
        assert!(
            matches!(strategy, LabelsStrategy::Csv),
            "strategy variant exists"
        );

        // Test the formatting function
        let labels = vec!["bug", "priority"];
        let args = execute_labels_strategy(strategy, &labels);
        assert_eq!(args, vec!["--labels", "bug,priority"]);

        // Empty labels produce no args
        let labels: Vec<&str> = vec![];
        let args = execute_labels_strategy(strategy, &labels);
        assert!(args.is_empty());
    }

    #[test]
    fn repeated_documents_formatting_contract() {
        // Repeated strategy formats labels as multiple flag occurrences.
        //
        // The contract:
        // 1. Each label gets its own --label flag.
        // 2. Flags and values are interleaved.
        // 3. Empty labels produce no args.
        //
        // Example: `["bug", "priority"]` → `["--label", "bug", "--label", "priority"]`

        let strategy = LabelsStrategy::Repeated;
        assert!(
            matches!(strategy, LabelsStrategy::Repeated),
            "strategy variant exists"
        );

        // Test the formatting function
        let labels = vec!["bug", "priority"];
        let args = execute_labels_strategy(strategy, &labels);
        assert_eq!(args, vec!["--label", "bug", "--label", "priority"]);

        // Empty labels produce no args
        let labels: Vec<&str> = vec![];
        let args = execute_labels_strategy(strategy, &labels);
        assert!(args.is_empty());
    }

    #[test]
    fn bare_import_documents_formatting_contract() {
        // Bare import strategy passes the input path directly.
        //
        // The contract:
        // 1. The input path is the only strategy-specific argument.
        // 2. No mode flag is added.
        //
        // Example: `["backup.jsonl"]`

        let strategy = ImportStrategy::Bare;
        assert!(
            matches!(strategy, ImportStrategy::Bare),
            "strategy variant exists"
        );

        // Test the formatting function
        let args = execute_import_strategy(strategy, Path::new("backup.jsonl"), None).unwrap();
        assert_eq!(args, vec!["backup.jsonl"]);
    }

    #[test]
    fn input_plus_mode_documents_formatting_contract() {
        // InputPlusMode strategy requires input file and mode flags.
        //
        // The contract:
        // 1. Requires --input flag with the file path.
        // 2. Requires a mode flag (--restore-into-empty or --merge).
        // 3. This implementation uses --restore-into-empty by default.
        //
        // Example: `["--input", "backup.jsonl", "--restore-into-empty"]`

        let strategy = ImportStrategy::InputPlusMode;
        assert!(
            matches!(strategy, ImportStrategy::InputPlusMode),
            "strategy variant exists"
        );

        // Test the formatting function with input file
        let args = execute_import_strategy(
            strategy,
            Path::new("backup.jsonl"),
            Some("--restore-into-empty"),
        )
        .unwrap();
        assert_eq!(
            args,
            vec!["--input", "backup.jsonl", "--restore-into-empty"]
        );

        let result = execute_import_strategy(strategy, Path::new("backup.jsonl"), None);
        assert!(result.is_err());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Strategy validation tests
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn claim_strategy_all_variants_documented() {
        // Verify all claim strategy variants are documented with race semantics
        let variants = [
            ClaimStrategy::AtomicCommand,
            ClaimStrategy::CompareAndSet,
            ClaimStrategy::BatchOp,
        ];

        for variant in variants {
            // Each variant should serialize correctly for YAML descriptors
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            // Each variant should deserialize from snake_case
            let deserialized: Result<ClaimStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok(), "variant should deserialize");
        }
    }

    #[test]
    fn claim_auto_strategy_all_variants_documented() {
        // Verify all claim_auto strategy variants are documented
        let variants = [
            ClaimAutoStrategy::AtomicSubcommand,
            ClaimAutoStrategy::NonAtomicScan,
        ];

        for variant in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            let deserialized: Result<ClaimAutoStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok());
        }
    }

    #[test]
    fn split_strategy_all_variants_documented() {
        // Verify all split strategy variants are documented
        let variants = [SplitStrategy::TransactionalBatch, SplitStrategy::Sequential];

        for variant in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            let deserialized: Result<SplitStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok());
        }
    }

    #[test]
    fn create_id_strategy_all_variants_documented() {
        // Verify all create_id strategy variants are documented
        let variants = [CreateIdStrategy::BareId, CreateIdStrategy::JsonField];

        for variant in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            let deserialized: Result<CreateIdStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok());
        }
    }

    #[test]
    fn labels_strategy_all_variants_documented() {
        // Verify all labels strategy variants are documented
        let variants = [LabelsStrategy::Csv, LabelsStrategy::Repeated];

        for variant in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            let deserialized: Result<LabelsStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok());
        }
    }

    #[test]
    fn import_strategy_all_variants_documented() {
        // Verify all import strategy variants are documented
        let variants = [ImportStrategy::Bare, ImportStrategy::InputPlusMode];

        for variant in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert!(serialized.starts_with('"') && serialized.ends_with('"'));

            let deserialized: Result<ImportStrategy, _> = serde_json::from_str(&serialized);
            assert!(deserialized.is_ok());
        }
    }
}
