// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::hash::{Hash, Hasher};

#[derive(Eq, PartialEq, Hash)]
pub struct StackTrace {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id\[0\].
    pub locations: Vec<LocationId>,
}

/// Borrowed lookup key for `FxIndexSet<StackTrace>` that hashes identically to `StackTrace`
/// without allocating a `Vec<LocationId>`.
#[derive(Eq, PartialEq)]
pub(crate) struct StackTraceRef<'a>(pub(crate) &'a [LocationId]);

impl Hash for StackTraceRef<'_> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl indexmap::Equivalent<StackTrace> for StackTraceRef<'_> {
    #[inline]
    fn equivalent(&self, key: &StackTrace) -> bool {
        self.0 == key.locations.as_slice()
    }
}

impl Item for StackTrace {
    type Id = StackTraceId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(C)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct StackTraceId(u32);

impl Id for StackTraceId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        let index: u32 = inner.try_into().expect("StackTraceId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}

impl From<StackTraceId> for u32 {
    fn from(value: StackTraceId) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    #[test]
    fn stack_trace_ref_hashes_identically_to_stack_trace() {
        let hasher = std::hash::BuildHasherDefault::<rustc_hash::FxHasher>::default();
        let locs = vec![
            LocationId::from_offset(0),
            LocationId::from_offset(42),
            LocationId::from_offset(255),
        ];
        let owned = StackTrace {
            locations: locs.clone(),
        };
        assert_eq!(hasher.hash_one(&owned), hasher.hash_one(StackTraceRef(&locs)));
        assert_eq!(
            hasher.hash_one(&StackTrace { locations: vec![] }),
            hasher.hash_one(StackTraceRef(&[]))
        );
    }
}
