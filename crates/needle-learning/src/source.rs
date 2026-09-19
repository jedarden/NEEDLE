//! Read-only source contracts implemented by NEEDLE controllers.

use crate::factory_types::{Attempt, AttemptId, ContextManifest, EvidenceBundle, Resolution};

/// A source can only return an immutable snapshot for a key.
///
/// The trait intentionally has no `&mut self` method, persistence method, or
/// lifecycle operation. Implementations may read a journal, store, or cache in
/// the controller crate, but the kernel sees only this contract.
pub trait ReadOnlySource {
    /// Lookup key.
    type Key: ?Sized;
    /// Snapshot returned by the source.
    type Value: Clone;

    /// Read one snapshot. Missing data is explicit and is not treated as
    /// success by kernel operations.
    fn read(&self, key: &Self::Key) -> Option<Self::Value>;
}

/// Read-only attempt source.
pub trait AttemptSource: ReadOnlySource<Key = AttemptId, Value = Attempt> {}

impl<T> AttemptSource for T where T: ReadOnlySource<Key = AttemptId, Value = Attempt> {}

/// Read-only context-manifest source.
pub trait ContextSource: ReadOnlySource<Key = AttemptId, Value = ContextManifest> {}

impl<T> ContextSource for T where T: ReadOnlySource<Key = AttemptId, Value = ContextManifest> {}

/// Read-only evidence source.
pub trait EvidenceSource: ReadOnlySource<Key = AttemptId, Value = EvidenceBundle> {}

impl<T> EvidenceSource for T where T: ReadOnlySource<Key = AttemptId, Value = EvidenceBundle> {}

/// Read-only resolution source.
pub trait ResolutionSource: ReadOnlySource<Key = AttemptId, Value = Resolution> {}

impl<T> ResolutionSource for T where T: ReadOnlySource<Key = AttemptId, Value = Resolution> {}
