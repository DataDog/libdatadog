// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! An append-only, fixed-capacity Swiss table that lives in caller-provided
//! bytes.
//!
//! The table never allocates, never resizes, and never deletes. It only
//! indexes: it stores small plain-data buckets (typically IDs, or an arena
//! offset plus an ID), and the caller supplies the hash and the equality test,
//! like `hashbrown::HashTable`. Growth is the caller's job: when a table is
//! full, the caller moves on to a new table in other bytes.
//!
//! The same table serves per-profile indexes, which are owned by one thread,
//! and the shared dictionary's regions, where the dictionary provides the
//! locking. This module knows nothing about either.
//!
//! # Geometry
//!
//! A table has `groups` groups of [`GROUP_WIDTH`] buckets each, so the bucket
//! count is a multiple of 16 but not necessarily a power of two. The first
//! group is chosen by a multiply-shift of the hash, so no division is needed
//! for any group count. Each group's 16 control bytes are scanned at once with
//! SSE2 or NEON. The table accepts at most [`GROUP_CAPACITY`] entries per
//! group on average, exactly 7/8 of its buckets, so a probe always reaches an
//! empty bucket. Individual groups may still fill completely.
//!
//! The bytes hold all the control bytes first, then all the buckets:
//!
//! ```text
//! [ 16 × groups control bytes ][ 16 × groups buckets of B ]
//! ```
//!
//! The start must be 16-byte aligned. See [`bytes_for_groups`] and
//! [`groups_for_bytes`]. Zero-filled bytes are an empty table.
//!
//! # Synchronization
//!
//! The table has no locks and no atomics. [`HashTable`] needs exclusive access
//! (`&mut`), and [`HashTableRef`] is a read-only view (`&`). Callers that share
//! tables between threads or processes provide the exclusion themselves, and
//! only create a [`HashTableRef`] over bytes that are no longer written, such
//! as a sealed region.
//!
//! # Hashes
//!
//! As in hashbrown, the top 7 bits of the hash become the control byte's tag,
//! and the low 57 bits pick the first group. The two never share bits, and
//! both need good entropy.

mod group;
#[cfg(test)]
mod tests;

use core::fmt;
use group::BitMaskIter;
pub use group::{GROUP_WIDTH, Group};

/// Maximum entries per group of 16 buckets: the 7/8 load limit.
pub const GROUP_CAPACITY: usize = 14;

/// Plain data that can be stored as a bucket in caller-provided bytes.
///
/// # Safety
///
/// Every bit pattern must be a valid value of the type: bucket bytes are never
/// zeroed, and a reopened table reads whatever bytes it was given. The type
/// must have no padding and an alignment of at most 16. `Copy` rules out drop
/// glue.
pub unsafe trait Bucket: Copy {}

// SAFETY: integers and arrays of integers accept every bit pattern, have no
// padding, and are aligned to at most 8 bytes.
unsafe impl Bucket for u32 {}
// SAFETY: as above.
unsafe impl Bucket for u64 {}
// SAFETY: as above.
unsafe impl Bucket for [u32; 2] {}

/// Why the given bytes cannot hold a table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    /// The bytes do not start on a 16-byte boundary.
    Misaligned,
    /// The bytes are shorter than [`bytes_for_groups`] requires.
    TooSmall,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Misaligned => "hash table bytes are not 16-byte aligned",
            Self::TooSmall => "too few bytes for the requested hash table groups",
        })
    }
}

impl core::error::Error for LayoutError {}

/// Bytes needed per group: 16 control bytes plus 16 buckets.
#[inline]
#[must_use]
pub const fn bytes_per_group<B: Bucket>() -> usize {
    GROUP_WIDTH * (1 + size_of::<B>())
}

/// Bytes needed for a table of `groups` groups, or `None` on overflow.
#[inline]
#[must_use]
pub const fn bytes_for_groups<B: Bucket>(groups: usize) -> Option<usize> {
    groups.checked_mul(bytes_per_group::<B>())
}

/// The largest group count whose table fits in `bytes` bytes.
#[inline]
#[must_use]
pub const fn groups_for_bytes<B: Bucket>(bytes: usize) -> usize {
    bytes / bytes_per_group::<B>()
}

/// The maximum number of entries in a table of `groups` groups.
#[inline]
#[must_use]
pub const fn capacity_for_groups(groups: usize) -> usize {
    groups.saturating_mul(GROUP_CAPACITY)
}

type Buckets<B> = [B; GROUP_WIDTH];

/// A table with exclusive access to its bytes.
pub struct HashTable<'a, B: Bucket> {
    controls: &'a mut [Group],
    buckets: &'a mut [Buckets<B>],
    len: usize,
}

/// A read-only view of a table, for bytes that no longer change.
#[derive(Clone, Copy)]
pub struct HashTableRef<'a, B: Bucket> {
    controls: &'a [Group],
    buckets: &'a [Buckets<B>],
}

/// The result of looking up a key for insertion.
#[derive(Debug)]
pub enum Entry<'t, B: Bucket> {
    /// The key is present.
    Occupied(&'t B),
    /// The key is absent and there is room to insert it.
    Vacant(VacantEntry<'t, B>),
    /// The key is absent and the table is at its 7/8 capacity.
    Full,
}

/// An empty bucket reserved for one key. Inserting consumes the entry, and
/// dropping it leaves the table unchanged.
#[derive(Debug)]
pub struct VacantEntry<'t, B: Bucket> {
    control: &'t mut u8,
    bucket: &'t mut B,
    len: &'t mut usize,
    tag: u8,
}

impl<B: Bucket> VacantEntry<'_, B> {
    /// Stores `value` in the reserved bucket. The bucket is written before its
    /// control byte, so a control byte never refers to an unwritten bucket.
    #[inline]
    pub const fn insert(self, value: B) {
        *self.bucket = value;
        *self.control = self.tag;
        *self.len += 1;
    }
}

/// Where a probe ended.
enum Probe {
    /// A bucket matched: (group, index).
    Found(usize, usize),
    /// No match; this is the first empty bucket on the probe path.
    Vacant(usize, usize),
    /// Every group was full, or the table has no groups.
    Exhausted,
}

impl<'a, B: Bucket> HashTable<'a, B> {
    /// A table with no groups and no bytes. It is always full, which makes it
    /// a placeholder until memory is available.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            controls: &mut [],
            buckets: &mut [],
            len: 0,
        }
    }

    /// Creates an empty table with as many groups as fit in `bytes`
    /// ([`groups_for_bytes`]), zeroing the control bytes. To get exactly `n`
    /// groups, pass exactly `bytes_for_groups(n)` bytes ([`bytes_for_groups`]).
    /// Too few bytes for one group gives a table with no groups, which is
    /// always full.
    ///
    /// # Errors
    ///
    /// [`LayoutError::Misaligned`] if `bytes` does not start 16-byte aligned.
    pub fn new(bytes: &'a mut [u8]) -> Result<Self, LayoutError> {
        let mut table = Self::from_zeroed(bytes)?;
        table.clear();
        Ok(table)
    }

    /// Like [`Self::new`], but for bytes whose control-byte prefix is already
    /// zero, such as memory from an allocator's `allocate_zeroed` or a fresh
    /// OS mapping. It skips zeroing them a second time.
    ///
    /// Nonzero control bytes are memory-safe but make the table's contents
    /// meaningless.
    ///
    /// # Errors
    ///
    /// [`LayoutError::Misaligned`] if `bytes` does not start 16-byte aligned.
    pub fn from_zeroed(bytes: &'a mut [u8]) -> Result<Self, LayoutError> {
        let groups = groups_for_bytes::<B>(bytes.len());
        Self::reopen(bytes, groups, 0)
    }

    /// Adopts a table that already exists in `bytes`, for example one written
    /// earlier or a shared region written by another process.
    ///
    /// `groups` must equal the group count the table was created with: it
    /// decides where every probe starts, so a different count makes existing
    /// keys unfindable. It is therefore stated explicitly rather than derived
    /// from `bytes.len()`. Bytes past the table are not used.
    ///
    /// `len` is the caller's count of occupied buckets, used only to enforce
    /// the capacity limit. Any contents are memory-safe; contents that this
    /// module did not write give meaningless lookups.
    ///
    /// # Errors
    ///
    /// [`LayoutError::Misaligned`] if `bytes` does not start 16-byte aligned,
    /// and [`LayoutError::TooSmall`] if it is shorter than
    /// `bytes_for_groups::<B>(groups)` ([`bytes_for_groups`]).
    pub fn reopen(bytes: &'a mut [u8], groups: usize, len: usize) -> Result<Self, LayoutError> {
        let (controls, buckets) = split_mut::<B>(bytes, groups)?;
        Ok(Self {
            controls,
            buckets,
            len,
        })
    }

    /// The number of entries.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the table has no entries.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The maximum number of entries: 7/8 of the buckets.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        capacity_for_groups(self.controls.len())
    }

    /// Whether the table is at capacity. A full table still finds its keys.
    #[inline]
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len >= self.capacity()
    }

    /// The number of groups.
    #[inline]
    #[must_use]
    pub const fn groups(&self) -> usize {
        self.controls.len()
    }

    /// A read-only view of this table.
    #[inline]
    #[must_use]
    pub const fn view(&self) -> HashTableRef<'_, B> {
        HashTableRef {
            controls: self.controls,
            buckets: self.buckets,
        }
    }

    /// Finds the bucket for which `eq` returns true, among buckets whose
    /// stored tag matches `hash`.
    #[inline]
    pub fn find(&self, hash: u64, eq: impl FnMut(&B) -> bool) -> Option<&B> {
        self.view().find(hash, eq)
    }

    /// Finds the key, or reserves an empty bucket for it. Work that is only
    /// needed for a new key, such as copying a string into an arena, can be
    /// done before [`VacantEntry::insert`].
    pub fn entry(&mut self, hash: u64, eq: impl FnMut(&B) -> bool) -> Entry<'_, B> {
        match probe(self.controls, self.buckets, hash, eq) {
            Probe::Found(group, index) => {
                match self.buckets.get(group).and_then(|b| b.get(index)) {
                    Some(bucket) => Entry::Occupied(bucket),
                    None => Entry::Full,
                }
            }
            Probe::Vacant(group, index) if self.len < self.capacity() => {
                let control = self
                    .controls
                    .get_mut(group)
                    .and_then(|c| c.0.get_mut(index));
                let bucket = self.buckets.get_mut(group).and_then(|b| b.get_mut(index));
                match (control, bucket) {
                    (Some(control), Some(bucket)) => Entry::Vacant(VacantEntry {
                        control,
                        bucket,
                        len: &mut self.len,
                        tag: tag(hash),
                    }),
                    _ => Entry::Full,
                }
            }
            Probe::Vacant(..) | Probe::Exhausted => Entry::Full,
        }
    }

    /// Inserts `value` without looking for an equal key.
    ///
    /// # Errors
    ///
    /// Hands `value` back, leaving the table unchanged, if the table is full.
    pub fn insert_unique(&mut self, hash: u64, value: B) -> Result<(), B> {
        match self.entry(hash, |_| false) {
            Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
            Entry::Occupied(_) | Entry::Full => Err(value),
        }
    }

    /// Removes every entry by zeroing the control bytes. Buckets are left as
    /// they are.
    pub fn clear(&mut self) {
        self.controls.fill(Group::EMPTY);
        self.len = 0;
    }

    /// Iterates over the occupied buckets in storage order.
    #[inline]
    #[must_use]
    pub fn iter(&self) -> Iter<'_, B> {
        self.view().iter()
    }
}

impl<B: Bucket> fmt::Debug for HashTable<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashTable")
            .field("groups", &self.groups())
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl<'a, B: Bucket> HashTableRef<'a, B> {
    /// A view of a table with no groups: it finds nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            controls: &[],
            buckets: &[],
        }
    }

    /// A read-only view of an existing table of `groups` groups in `bytes`.
    /// Any contents are memory-safe.
    ///
    /// # Errors
    ///
    /// As for [`HashTable::reopen`].
    pub fn new(bytes: &'a [u8], groups: usize) -> Result<Self, LayoutError> {
        let (controls, buckets) = split::<B>(bytes, groups)?;
        Ok(Self { controls, buckets })
    }

    /// The number of groups.
    #[inline]
    #[must_use]
    pub const fn groups(&self) -> usize {
        self.controls.len()
    }

    /// Finds the bucket for which `eq` returns true, among buckets whose
    /// stored tag matches `hash`.
    #[inline]
    pub fn find(&self, hash: u64, eq: impl FnMut(&B) -> bool) -> Option<&'a B> {
        match probe(self.controls, self.buckets, hash, eq) {
            Probe::Found(group, index) => self.buckets.get(group)?.get(index),
            Probe::Vacant(..) | Probe::Exhausted => None,
        }
    }

    /// Iterates over the occupied buckets in storage order.
    #[inline]
    #[must_use]
    pub fn iter(&self) -> Iter<'a, B> {
        Iter {
            controls: self.controls.iter(),
            buckets: self.buckets.iter(),
            current: None,
        }
    }
}

impl<'t, B: Bucket> IntoIterator for &'t HashTable<'_, B> {
    type Item = &'t B;
    type IntoIter = Iter<'t, B>;

    fn into_iter(self) -> Iter<'t, B> {
        self.iter()
    }
}

impl<'a, B: Bucket> IntoIterator for &HashTableRef<'a, B> {
    type Item = &'a B;
    type IntoIter = Iter<'a, B>;

    fn into_iter(self) -> Iter<'a, B> {
        self.iter()
    }
}

impl<B: Bucket> fmt::Debug for HashTableRef<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashTableRef")
            .field("groups", &self.groups())
            .finish_non_exhaustive()
    }
}

/// Iterator over occupied buckets, from [`HashTable::iter`].
#[derive(Clone)]
pub struct Iter<'a, B: Bucket> {
    controls: core::slice::Iter<'a, Group>,
    buckets: core::slice::Iter<'a, Buckets<B>>,
    current: Option<(BitMaskIter, &'a Buckets<B>)>,
}

impl<'a, B: Bucket> Iterator for Iter<'a, B> {
    type Item = &'a B;

    fn next(&mut self) -> Option<&'a B> {
        loop {
            if let Some((mask, buckets)) = &mut self.current
                && let Some(index) = mask.next()
            {
                return buckets.get(index);
            }
            let controls = self.controls.next()?;
            let buckets = self.buckets.next()?;
            self.current = Some((controls.match_full().into_iter(), buckets));
        }
    }
}

impl<B: Bucket> fmt::Debug for Iter<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Iter").finish_non_exhaustive()
    }
}

/// Bits of the hash that pick the start group; the rest form the tag.
const GROUP_BITS: u32 = 57;

/// The control byte for an occupied bucket: the high bit set, plus the top 7
/// bits of the hash.
#[inline]
const fn tag(hash: u64) -> u8 {
    let [.., top] = hash.to_le_bytes();
    0x80 | (top >> 1)
}

/// Maps the low 57 bits of `hash` onto `0..groups` without division.
#[inline]
fn start_group(hash: u64, groups: usize) -> usize {
    // usize is at most 64 bits on every supported target, so widening to
    // u128 is lossless. (h57 * groups) >> 57 < groups because h57 < 2^57, so
    // the result always converts back.
    let low = hash & ((1 << GROUP_BITS) - 1);
    let scaled = (u128::from(low) * groups as u128) >> GROUP_BITS;
    usize::try_from(scaled).unwrap_or(0)
}

#[inline]
const fn next_group(group: usize, groups: usize) -> usize {
    let next = group + 1;
    if next == groups { 0 } else { next }
}

/// Probes from the hash's start group until a bucket matches or a group with
/// an empty bucket ends the search.
#[inline]
fn probe<B: Bucket>(
    controls: &[Group],
    buckets: &[Buckets<B>],
    hash: u64,
    mut eq: impl FnMut(&B) -> bool,
) -> Probe {
    let tag = tag(hash);
    let groups = controls.len();
    let mut group = start_group(hash, groups);
    for _ in 0..groups {
        let (Some(group_controls), Some(group_buckets)) = (controls.get(group), buckets.get(group))
        else {
            return Probe::Exhausted;
        };
        for index in group_controls.match_tag(tag) {
            if let Some(bucket) = group_buckets.get(index)
                && eq(bucket)
            {
                return Probe::Found(group, index);
            }
        }
        // Candidates are checked before an empty bucket ends the search: a
        // match can share its group with empty buckets.
        if let Some(index) = group_controls.match_empty().lowest() {
            return Probe::Vacant(group, index);
        }
        group = next_group(group, groups);
    }
    Probe::Exhausted
}

/// Byte lengths of the control and bucket arrays, after checking alignment
/// and size.
fn layout<B: Bucket>(
    start: *const u8,
    len: usize,
    groups: usize,
) -> Result<(usize, usize), LayoutError> {
    const { assert!(align_of::<B>() <= align_of::<Group>()) };
    if start.align_offset(align_of::<Group>()) != 0 {
        return Err(LayoutError::Misaligned);
    }
    let controls = groups
        .checked_mul(GROUP_WIDTH)
        .ok_or(LayoutError::TooSmall)?;
    let buckets = controls
        .checked_mul(size_of::<B>())
        .ok_or(LayoutError::TooSmall)?;
    let total = controls.checked_add(buckets).ok_or(LayoutError::TooSmall)?;
    if len < total {
        return Err(LayoutError::TooSmall);
    }
    Ok((controls, buckets))
}

type Parts<'a, B> = (&'a mut [Group], &'a mut [Buckets<B>]);

fn split_mut<B: Bucket>(bytes: &mut [u8], groups: usize) -> Result<Parts<'_, B>, LayoutError> {
    let (controls_len, buckets_len) = layout::<B>(bytes.as_ptr(), bytes.len(), groups)?;
    let (controls, rest) = bytes
        .split_at_mut_checked(controls_len)
        .ok_or(LayoutError::TooSmall)?;
    let buckets = rest.get_mut(..buckets_len).ok_or(LayoutError::TooSmall)?;
    // SAFETY: `controls` starts 16-aligned and holds exactly `groups` × 16
    // bytes; Group is a 16-aligned [u8; 16], which accepts every value.
    // `buckets` starts a multiple of 16 bytes past an aligned start, so it is
    // aligned for B (align ≤ 16, asserted above), and holds exactly `groups`
    // × 16 × size_of::<B>() bytes. B accepts every bit pattern (Bucket's
    // contract). The two slices borrow disjoint parts of `bytes`.
    unsafe {
        Ok((
            core::slice::from_raw_parts_mut(controls.as_mut_ptr().cast(), groups),
            core::slice::from_raw_parts_mut(buckets.as_mut_ptr().cast(), groups),
        ))
    }
}

fn split<B: Bucket>(bytes: &[u8], groups: usize) -> Result<(&[Group], &[Buckets<B>]), LayoutError> {
    let (controls_len, buckets_len) = layout::<B>(bytes.as_ptr(), bytes.len(), groups)?;
    let (controls, rest) = bytes
        .split_at_checked(controls_len)
        .ok_or(LayoutError::TooSmall)?;
    let buckets = rest.get(..buckets_len).ok_or(LayoutError::TooSmall)?;
    // SAFETY: as in `split_mut`, for shared borrows.
    unsafe {
        Ok((
            core::slice::from_raw_parts(controls.as_ptr().cast(), groups),
            core::slice::from_raw_parts(buckets.as_ptr().cast(), groups),
        ))
    }
}
