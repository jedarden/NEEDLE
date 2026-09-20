//! Typed policy-source authority and deterministic precedence resolution.
//!
//! Policy precedence is deliberately derived from [`PolicyKind`].  Callers
//! cannot attach an arbitrary numeric priority to a source, so every resolver
//! uses this registry as the one authority for ordering policy inputs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Authority classes from highest to lowest precedence.
///
/// The order is represented by the implementation of [`Self::rank`] rather
/// than by values supplied by callers.  This keeps precedence a property of
/// the policy vocabulary, not of an individual source record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}
