// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A generic id for profile stacks, mappings, locations, functions, etc. It's
/// a handle-like type. For compatibility with OpenTelemetry which uses i32,
/// it's using a 31-bit range inside the u32. We don't need such large IDs,
/// so this saves some memory as well, although we may consider using a 32-bit
/// generation or something similar to solve the ABA type of problems.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProfileId(u32);

impl ProfileId {
    pub const ZERO: Self = Self(0);

    /// # Safety
    /// The id must fit into the least significant 31 bits.
    pub const unsafe fn new_unchecked(id: u32) -> Self {
        Self(id)
    }

    /// Returns u32 which is guaranteed to fit into an i32.
    #[inline(always)]
    pub const fn into_u32(self) -> u32 {
        self.0
    }

    // Needed for conversions to offsets in data structures.
    pub const fn into_usize(self) -> usize {
        self.into_u32() as usize
    }

    // Needed for pprof, which uses u64 for IDs.
    pub const fn into_u64(self) -> u64 {
        self.into_u32() as u64
    }

    // Needed for OpenTelemetry, which uses i32 for indices.
    pub const fn into_i32(self) -> i32 {
        self.into_u32() as i32
    }
}

impl From<ProfileId> for u32 {
    fn from(id: ProfileId) -> Self {
        id.0
    }
}

// Needed for pprof, which uses u64 for IDs.
impl From<ProfileId> for u64 {
    fn from(id: ProfileId) -> Self {
        u32::from(id) as u64
    }
}

// Needed for OpenTelemetry, which uses i32 for indices.
impl From<ProfileId> for i32 {
    fn from(id: ProfileId) -> Self {
        u32::from(id) as i32
    }
}
