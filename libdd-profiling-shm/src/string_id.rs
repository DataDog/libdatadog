// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! String identifier type for SHM string table entries.
//!
//! `ShmStringId` is stored as a `u32` but only uses the low 31 bits.
//! Valid values are `0..=0x7fff_ffff`.
//!
//! The most significant bit is intentionally reserved for future use.

/// Bit mask for the most significant bit (intentionally reserved for future use).
const HIGH_BIT: u32 = 1 << 31;

/// Maximum valid value for a 31-bit string id.
///
/// The most significant bit remains reserved for future metadata/expansion.
pub const MAX_STRING_ID_31BIT: u32 = !HIGH_BIT;

const _: () = assert!(MAX_STRING_ID_31BIT == 0x7fff_ffff);

/// An index into the SHM string directory.
///
/// Logically a 31-bit unsigned integer in the lower 31 bits.
/// Constructed via a fallible [`ShmStringId::new`] that rejects values with
/// the most significant bit set.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ShmStringId(u32);

impl ShmStringId {
    /// Creates a new `ShmStringId` if `value` fits in 31 bits.
    /// Returns `None` if the most significant bit is set.
    #[inline]
    pub fn new(value: u32) -> Option<Self> {
        if value & HIGH_BIT == 0 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Const constructor for well-known string IDs.
    #[inline]
    pub const fn new_const(value: u16) -> Self {
        Self(value as u32)
    }

    /// Returns the raw 31-bit index in the SHM domain.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_string_id_zero() {
        let id = ShmStringId::new(0).unwrap();
        assert_eq!(id.index(), 0);
    }

    #[test]
    fn shm_string_id_max_valid() {
        let max = MAX_STRING_ID_31BIT; // 2^31 - 1
        let id = ShmStringId::new(max).unwrap();
        assert_eq!(id.index(), max);
    }

    #[test]
    fn shm_string_id_rejects_high_bit() {
        assert!(ShmStringId::new(HIGH_BIT).is_none());
        assert!(ShmStringId::new(u32::MAX).is_none());
    }

    #[test]
    fn size_and_alignment() {
        assert_eq!(core::mem::size_of::<ShmStringId>(), 4);
        assert_eq!(core::mem::align_of::<ShmStringId>(), 4);
    }
}
