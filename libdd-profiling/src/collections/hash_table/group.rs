// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sixteen control bytes scanned together, using SSE2 on `x86_64`, NEON on
//! aarch64, and a scalar loop elsewhere.
//!
//! A control byte of zero means empty, and an occupied control is
//! `0x80 | (hash & 0x7f)`. Zero-filled memory is therefore an empty table.

/// Number of buckets that share one group of control bytes.
pub const GROUP_WIDTH: usize = 16;

/// The control byte of an empty bucket.
pub const EMPTY: u8 = 0;

/// Sixteen control bytes, aligned for a single SIMD load.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Group(pub [u8; GROUP_WIDTH]);

impl Group {
    /// A group whose sixteen buckets are all empty.
    pub const EMPTY: Self = Self([EMPTY; GROUP_WIDTH]);

    /// Buckets whose control byte equals `tag`.
    #[inline]
    #[must_use]
    pub fn match_tag(&self, tag: u8) -> BitMask {
        imp::match_byte(self, tag)
    }

    /// Buckets that are empty.
    #[inline]
    #[must_use]
    pub fn match_empty(&self) -> BitMask {
        imp::match_byte(self, EMPTY)
    }

    /// Buckets that are occupied: any control byte with its high bit set.
    #[inline]
    #[must_use]
    pub fn match_full(&self) -> BitMask {
        imp::match_high_bit(self)
    }
}

/// One bit per bucket of a group; bit `i` is bucket `i`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitMask(pub u16);

impl BitMask {
    /// The lowest set bit, if any. The result is always below [`GROUP_WIDTH`].
    #[inline]
    #[must_use]
    pub const fn lowest(self) -> Option<usize> {
        lowest_bit(self.0)
    }
}

impl IntoIterator for BitMask {
    type Item = usize;
    type IntoIter = BitMaskIter;

    #[inline]
    fn into_iter(self) -> BitMaskIter {
        BitMaskIter(self.0)
    }
}

/// The set bits of a [`BitMask`], lowest first. Each step clears the bit it
/// yields.
#[derive(Clone, Debug)]
pub struct BitMaskIter(u16);

impl Iterator for BitMaskIter {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        let bit = lowest_bit(self.0)?;
        self.0 &= self.0 - 1;
        Some(bit)
    }
}

#[inline]
const fn lowest_bit(bits: u16) -> Option<usize> {
    if bits == 0 {
        None
    } else {
        // A nonzero u16 has at most 15 trailing zeros; the mask documents it.
        Some(bits.trailing_zeros() as usize & (GROUP_WIDTH - 1))
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use super::{BitMask, Group};
    use core::arch::x86_64::{
        __m128i, _mm_cmpeq_epi8, _mm_load_si128, _mm_movemask_epi8, _mm_set1_epi8,
    };

    /// SAFETY (every unsafe block in this module): SSE2 is part of the `x86_64`
    /// baseline, so these intrinsics are always available; Group is 16
    /// initialized bytes with 16-byte alignment, as `_mm_load_si128` requires.
    #[inline]
    fn load(group: &Group) -> __m128i {
        // SAFETY: see the module note above.
        unsafe { _mm_load_si128(core::ptr::from_ref(group).cast()) }
    }

    /// `_mm_movemask_epi8` sets only the low 16 bits (one per lane).
    #[inline]
    #[allow(clippy::cast_possible_truncation, reason = "movemask fits in 16 bits")]
    const fn low16(mask: i32) -> BitMask {
        BitMask(mask.cast_unsigned() as u16)
    }

    #[inline]
    pub(super) fn match_byte(group: &Group, byte: u8) -> BitMask {
        // SAFETY: see the note on `load`.
        low16(unsafe {
            _mm_movemask_epi8(_mm_cmpeq_epi8(
                load(group),
                _mm_set1_epi8(byte.cast_signed()),
            ))
        })
    }

    #[inline]
    pub(super) fn match_high_bit(group: &Group) -> BitMask {
        // SAFETY: see the note on `load`.
        low16(unsafe { _mm_movemask_epi8(load(group)) })
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use super::{BitMask, Group};
    use core::arch::aarch64::{
        uint8x16_t, vaddv_u8, vandq_u8, vceqq_u8, vcltzq_s8, vcombine_u8, vcreate_u8, vdupq_n_u8,
        vget_high_u8, vget_low_u8, vld1q_u8, vreinterpretq_s8_u8,
    };

    /// SAFETY (every unsafe block in this module): NEON is part of the
    /// aarch64 baseline, so these intrinsics are always available; Group is 16
    /// initialized, readable bytes, as `vld1q_u8` requires.
    #[inline]
    fn load(group: &Group) -> uint8x16_t {
        // SAFETY: see the module note above.
        unsafe { vld1q_u8(group.0.as_ptr()) }
    }

    /// Packs lanes that are all-ones or all-zeros into one bit per lane.
    #[inline]
    fn pack(lanes: uint8x16_t) -> BitMask {
        // Lane i of each half keeps weight 1 << i, then each half sums to a byte.
        // SAFETY: see the note on `load`.
        let (low, high) = unsafe {
            let weights = vcreate_u8(0x8040_2010_0804_0201);
            let weighted = vandq_u8(lanes, vcombine_u8(weights, weights));
            (
                vaddv_u8(vget_low_u8(weighted)),
                vaddv_u8(vget_high_u8(weighted)),
            )
        };
        BitMask(u16::from(low) | (u16::from(high) << 8))
    }

    #[inline]
    pub(super) fn match_byte(group: &Group, byte: u8) -> BitMask {
        // SAFETY: see the note on `load`.
        pack(unsafe { vceqq_u8(load(group), vdupq_n_u8(byte)) })
    }

    #[inline]
    pub(super) fn match_high_bit(group: &Group) -> BitMask {
        // SAFETY: see the note on `load`.
        pack(unsafe { vcltzq_s8(vreinterpretq_s8_u8(load(group))) })
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    pub(super) use super::scalar::{match_byte, match_high_bit};
}

/// Portable reference implementation. It is the fallback on other targets and
/// the oracle the SIMD paths are tested against.
#[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
pub mod scalar {
    use super::{BitMask, Group};

    #[inline]
    pub fn match_byte(group: &Group, byte: u8) -> BitMask {
        let mut mask = 0_u16;
        for (lane, &control) in group.0.iter().enumerate() {
            mask |= u16::from(control == byte) << lane;
        }
        BitMask(mask)
    }

    #[inline]
    pub fn match_high_bit(group: &Group) -> BitMask {
        let mut mask = 0_u16;
        for (lane, &control) in group.0.iter().enumerate() {
            mask |= u16::from(control >> 7) << lane;
        }
        BitMask(mask)
    }
}
