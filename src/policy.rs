//! Typed policy-source authority and deterministic precedence resolution.
//!
//! Policy precedence is deliberately derived from [`PolicyKind`].  Callers
//! cannot attach an arbitrary numeric priority to a source, so every resolver
//! uses this registry as the one authority for ordering policy inputs.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use needle_learning::ContextManifest;
use needle_learning::{
    AttemptId, Digest as LearningDigest, PolicyIdentity, SourceId, CURRENT_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Version of the canonical effective-policy representation.
pub const EFFECTIVE_POLICY_CANONICAL_VERSION: u8 = 1;

/// Schema version used when a resolved policy becomes a context manifest.
pub const CONTEXT_MANIFEST_SCHEMA_VERSION: u16 = CURRENT_SCHEMA_VERSION;

/// Stable identity assigned to the effective policy snapshot in a manifest.
pub const RESOLVED_POLICY_SOURCE_ID: &str = "needle.resolved-policy";

/// Version label for the effective policy snapshot recorded in a manifest.
pub const RESOLVED_POLICY_VERSION: &str = "effective-policy-v1";

/// Authority classes from highest to lowest precedence.
///
/// The order is represented by the implementation of [`Self::rank`] rather
/// than by values supplied by callers.  This keeps precedence a property of
/// the policy vocabulary, not of an individual source record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// Safety and constraints imposed by an external authority.
    Safety,
    /// Instructions from the repository and its nested directories.
    Repository,
    /// Instructions projected for a particular adapter.
    Adapter,
    /// Executable gates and workspace configuration.
    Gate,
    /// Accepted architectural decisions incorporated into the plan.
    Adr,
    /// The current plan and task acceptance criteria.
    Plan,
    /// Retrieved memory and candidate lessons; advisory only.
    Memory,
}

impl Authority {
    /// Return the precedence rank, where a larger rank wins.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Safety => 7,
            Self::Repository => 6,
            Self::Adapter => 5,
            Self::Gate => 4,
            Self::Adr => 3,
            Self::Plan => 2,
            Self::Memory => 1,
        }
    }
}

/// Authorities that must be present before an execution may be admitted.
///
/// The set is supplied by the execution surface instead of being inferred
/// from whichever policy sources happened to be discovered.  That distinction
/// is important for fail-closed behavior: an empty or partial registry cannot
/// accidentally authorize execution merely because resolution itself
/// succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecutionPolicyRequirements {
    authorities: Vec<Authority>,
}

impl ExecutionPolicyRequirements {
    /// Build requirements from an authority iterator.
    ///
    /// Authorities are sorted by descending precedence and duplicate entries
    /// are removed, making missing-authority failures deterministic even when
    /// callers assemble the requirements from different collection types.
    pub fn new<I>(authorities: I) -> Self
    where
        I: IntoIterator<Item = Authority>,
    {
        let mut authorities = authorities.into_iter().collect::<Vec<_>>();
        authorities.sort_by_key(|authority| std::cmp::Reverse(authority.rank()));
        authorities.dedup();
        Self { authorities }
    }

    /// The baseline authority envelope for an executable dispatch.
    ///
    /// Memory, the current plan, and accepted ADRs can enrich a context, but
    /// they do not replace the safety, repository, adapter, and executable
    /// gate authorities needed to run work.
    pub fn for_execution() -> Self {
        Self::new([
            Authority::Safety,
            Authority::Repository,
            Authority::Adapter,
            Authority::Gate,
        ])
    }

    pub fn authorities(&self) -> &[Authority] {
        &self.authorities
    }
}

/// A policy failure that prevents execution admission.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyAdmissionError {
    #[error(
        "execution requires policy authority {authority:?}, but no applicable source is present"
    )]
    MissingAuthority { authority: Authority },
    #[error(
        "required policy authority {authority:?} is ambiguous at scope {scope:?}: sources {sources:?}"
    )]
    AmbiguousAuthority {
        authority: Authority,
        scope: PolicyScope,
        sources: Vec<PolicySourceId>,
    },
    #[error("policy resolution failed during execution admission: {0}")]
    Resolution(#[from] PolicyError),
}

/// The closed set of policy source types understood by NEEDLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    ExternalConstraint,
    RepositoryInstructions,
    AdapterInstructions,
    ExecutableGate,
    AcceptedAdr,
    CurrentPlan,
    AdvisoryMemory,
}

impl PolicyKind {
    /// Derive the authority for this kind of source.
    pub const fn authority(self) -> Authority {
        match self {
            Self::ExternalConstraint => Authority::Safety,
            Self::RepositoryInstructions => Authority::Repository,
            Self::AdapterInstructions => Authority::Adapter,
            Self::ExecutableGate => Authority::Gate,
            Self::AcceptedAdr => Authority::Adr,
            Self::CurrentPlan => Authority::Plan,
            Self::AdvisoryMemory => Authority::Memory,
        }
    }
}

/// The scope in which a source is applicable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PolicyScope {
    /// Applies to every resolution.
    Global,
    /// Applies to a repository and all paths below its root.
    Repository { root: PathBuf },
    /// Applies to one nested directory and all paths below it.
    Directory { path: PathBuf },
    /// Applies only when the named adapter is selected.
    Adapter { name: String },
}

impl PolicyScope {
    pub fn repository(root: impl Into<PathBuf>) -> Self {
        Self::Repository { root: root.into() }
    }

    pub fn directory(path: impl Into<PathBuf>) -> Self {
        Self::Directory { path: path.into() }
    }

    pub fn adapter(name: impl Into<String>) -> Self {
        Self::Adapter { name: name.into() }
    }

    fn applies_to(&self, context: &ResolutionContext) -> bool {
        match self {
            Self::Global => true,
            Self::Repository { root } => context.target.starts_with(root),
            Self::Directory { path } => context.target.starts_with(path),
            Self::Adapter { name } => context.adapter.as_deref() == Some(name.as_str()),
        }
    }

    fn specificity(&self) -> usize {
        match self {
            Self::Global => 0,
            Self::Adapter { .. } => 1,
            Self::Repository { root } => 10 + root.components().count(),
            Self::Directory { path } => 10 + path.components().count(),
        }
    }
}

/// The inputs that determine whether a source applies to one resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionContext {
    pub target: PathBuf,
    pub adapter: Option<String>,
}

impl ResolutionContext {
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Self {
            target: target.into(),
            adapter: None,
        }
    }

    pub fn for_adapter(mut self, adapter: impl Into<String>) -> Self {
        self.adapter = Some(adapter.into());
        self
    }
}

/// Stable identity for one policy source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PolicySourceId(String);

impl PolicySourceId {
    pub fn new(id: impl Into<String>) -> Result<Self, PolicyError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(PolicyError::EmptySourceId);
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One typed, source-addressed policy input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySource {
    pub id: PolicySourceId,
    pub kind: PolicyKind,
    pub scope: PolicyScope,
    pub content: String,
}

impl PolicySource {
    pub fn new(
        id: impl Into<String>,
        kind: PolicyKind,
        scope: PolicyScope,
        content: impl Into<String>,
    ) -> Result<Self, PolicyError> {
        Ok(Self {
            id: PolicySourceId::new(id)?,
            kind,
            scope,
            content: content.into(),
        })
    }

    pub fn authority(&self) -> Authority {
        self.kind.authority()
    }
}

/// One registry owns every source considered by precedence resolution.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityRegistry {
    sources: BTreeMap<PolicySourceId, PolicySource>,
}

impl AuthorityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, source: PolicySource) -> Result<(), PolicyError> {
        if self.sources.contains_key(&source.id) {
            return Err(PolicyError::DuplicateSource {
                id: source.id.clone(),
            });
        }
        self.sources.insert(source.id.clone(), source);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Resolve applicable sources in highest-precedence order.
    ///
    /// Scope specificity breaks ties within an authority, so a nested
    /// directory source precedes its repository source.  Equal-authority,
    /// equal-scope sources with different content are ambiguous and fail
    /// deterministically instead of allowing insertion order to decide.
    pub fn resolve<'a>(
        &'a self,
        context: &ResolutionContext,
    ) -> Result<ResolvedPolicy<'a>, PolicyError> {
        let mut sources = self
            .sources
            .values()
            .filter(|source| source.scope.applies_to(context))
            .collect::<Vec<_>>();

        sources.sort_by(|left, right| {
            right
                .authority()
                .rank()
                .cmp(&left.authority().rank())
                .then_with(|| right.scope.specificity().cmp(&left.scope.specificity()))
                .then_with(|| left.id.cmp(&right.id))
        });

        for (index, left) in sources.iter().enumerate() {
            for right in sources.iter().skip(index + 1) {
                if left.authority() == right.authority()
                    && left.scope == right.scope
                    && left.content != right.content
                {
                    let (first, second) = if left.id <= right.id {
                        (left.id.clone(), right.id.clone())
                    } else {
                        (right.id.clone(), left.id.clone())
                    };
                    return Err(PolicyError::Conflict {
                        authority: left.authority(),
                        scope: left.scope.clone(),
                        sources: vec![first, second],
                    });
                }
            }
        }

        Ok(ResolvedPolicy { sources })
    }

    /// Admit an execution only when every required authority is represented
    /// by an applicable source and resolution found no contradiction.
    ///
    /// This is the execution boundary for policy.  Callers must use the
    /// returned resolved policy to build the execution context; an unresolved
    /// or incomplete registry never yields an admitted value.
    pub fn admit_execution<'a>(
        &'a self,
        context: &ResolutionContext,
        requirements: &ExecutionPolicyRequirements,
    ) -> Result<ResolvedPolicy<'a>, PolicyAdmissionError> {
        let resolved = self.resolve(context).map_err(|error| match error {
            PolicyError::Conflict {
                authority,
                scope,
                sources,
            } => PolicyAdmissionError::AmbiguousAuthority {
                authority,
                scope,
                sources,
            },
            other => PolicyAdmissionError::Resolution(other),
        })?;

        for authority in requirements.authorities() {
            if !resolved
                .sources()
                .iter()
                .any(|source| source.authority() == *authority)
            {
                return Err(PolicyAdmissionError::MissingAuthority {
                    authority: *authority,
                });
            }
        }

        Ok(resolved)
    }

    /// Resolve the applicable policy and materialize its immutable context.
    ///
    /// Resolution happens before materialization, so an ambiguous policy
    /// never produces a manifest that could be mistaken for an admitted
    /// context.
    pub fn materialize_context_manifest(
        &self,
        context: &ResolutionContext,
        attempt_id: AttemptId,
    ) -> Result<ContextManifest, PolicyError> {
        self.resolve(context)?
            .materialize_context_manifest(attempt_id)
    }
}

/// The ordered view of the registry for one resolution context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPolicy<'a> {
    sources: Vec<&'a PolicySource>,
}

impl<'a> ResolvedPolicy<'a> {
    pub fn sources(&self) -> &[&'a PolicySource] {
        &self.sources
    }

    pub fn winning_source(&self) -> Option<&'a PolicySource> {
        self.sources.first().copied()
    }

    pub fn source_ids(&self) -> Vec<&str> {
        self.sources
            .iter()
            .map(|source| source.id.as_str())
            .collect()
    }

    /// Return the stable JSON representation of the effective policy inputs.
    ///
    /// The resolver has already ordered these sources by authority, scope, and
    /// source ID. The canonical representation preserves that order and
    /// records source identity, precedence, scope, and a digest of each
    /// source's content. Raw policy content is intentionally not included in
    /// the representation because the resulting bytes may be persisted or
    /// attached to telemetry.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, PolicyError> {
        let canonical = CanonicalPolicyInputs {
            version: EFFECTIVE_POLICY_CANONICAL_VERSION,
            sources: self
                .sources
                .iter()
                .map(|source| CanonicalPolicySource::from(*source))
                .collect(),
        };

        serde_json::to_vec(&canonical).map_err(|error| PolicyError::Canonicalization {
            message: error.to_string(),
        })
    }

    /// Return the canonical effective-policy inputs as UTF-8 JSON.
    pub fn canonical_json(&self) -> Result<String, PolicyError> {
        let bytes = self.canonical_bytes()?;
        String::from_utf8(bytes).map_err(|error| PolicyError::Canonicalization {
            message: error.to_string(),
        })
    }

    /// Hash the canonical effective-policy inputs with SHA-256.
    pub fn hash(&self) -> Result<String, PolicyError> {
        let digest = Sha256::digest(self.canonical_bytes()?);
        Ok(format!("{digest:x}"))
    }

    /// Materialize the learning-kernel context record for this resolved
    /// policy snapshot.
    ///
    /// The manifest stores the digest of the canonical effective-policy
    /// representation rather than raw policy content. This preserves source
    /// identity, ordering, scope, and content hashes without leaking the
    /// instructions themselves into the attempt record.
    pub fn materialize_context_manifest(
        &self,
        attempt_id: AttemptId,
    ) -> Result<ContextManifest, PolicyError> {
        let policy_digest =
            LearningDigest::new(self.hash()?).map_err(|error| PolicyError::ManifestIdentity {
                message: error.to_string(),
            })?;

        Ok(ContextManifest {
            schema_version: CONTEXT_MANIFEST_SCHEMA_VERSION,
            attempt_id,
            policy: PolicyIdentity {
                source_id: SourceId::from_static(RESOLVED_POLICY_SOURCE_ID),
                digest: policy_digest,
                version: RESOLVED_POLICY_VERSION.to_owned(),
            },
            tools: Vec::new(),
            memory: Vec::new(),
            redactions: Vec::new(),
        })
    }
}

#[derive(Debug, Serialize)]
struct CanonicalPolicyInputs<'a> {
    version: u8,
    sources: Vec<CanonicalPolicySource<'a>>,
}

#[derive(Debug, Serialize)]
struct CanonicalPolicySource<'a> {
    id: &'a str,
    kind: PolicyKind,
    authority: Authority,
    precedence_rank: u8,
    scope: CanonicalPolicyScope,
    content_sha256: String,
}

impl<'a> From<&'a PolicySource> for CanonicalPolicySource<'a> {
    fn from(source: &'a PolicySource) -> Self {
        Self {
            id: source.id.as_str(),
            kind: source.kind,
            authority: source.authority(),
            precedence_rank: source.authority().rank(),
            scope: CanonicalPolicyScope::from(&source.scope),
            content_sha256: format!("{:x}", Sha256::digest(source.content.as_bytes())),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CanonicalPolicyScope {
    Global,
    Repository { root: String },
    Directory { path: String },
    Adapter { name: String },
}

impl From<&PolicyScope> for CanonicalPolicyScope {
    fn from(scope: &PolicyScope) -> Self {
        match scope {
            PolicyScope::Global => Self::Global,
            PolicyScope::Repository { root } => Self::Repository {
                root: root.to_string_lossy().into_owned(),
            },
            PolicyScope::Directory { path } => Self::Directory {
                path: path.to_string_lossy().into_owned(),
            },
            PolicyScope::Adapter { name } => Self::Adapter { name: name.clone() },
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy source ID must not be empty")]
    EmptySourceId,
    #[error("policy source {id:?} is already registered")]
    DuplicateSource { id: PolicySourceId },
    #[error("conflicting {authority:?} policy sources at scope {scope:?}: {sources:?}")]
    Conflict {
        authority: Authority,
        scope: PolicyScope,
        sources: Vec<PolicySourceId>,
    },
    #[error("failed to canonicalize effective policy inputs: {message}")]
    Canonicalization { message: String },
    #[error("failed to materialize context manifest identity: {message}")]
    ManifestIdentity { message: String },
}
