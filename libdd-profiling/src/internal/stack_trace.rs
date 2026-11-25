// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::internal::interning_api::{Generation, GenerationalId};
use anyhow::Context;
use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use std::hash::{BuildHasher, BuildHasherDefault};
use std::mem::MaybeUninit;

#[derive(Eq, PartialEq, Hash)]
pub struct StackTrace {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id\[0\].
    pub locations: Vec<LocationId>,
}

impl Item for StackTrace {
    type Id = StackTraceId;
}

/// Compact contiguous storage for deduplicated stack traces.
///
/// Each stack trace is stored once as canonical packed protobuf varints of its
/// 1-based `LocationId`s in a single byte slab (`data`), indexed by end offsets
/// (`ends`) and deduplicated by exact byte equality via `table`.
#[derive(Default)]
pub struct StackTraceStore {
    data: Vec<u8>,
    ends: Vec<u32>,
    table: HashTable<u32>,
    max_location_id: u32,
}

#[inline]
fn hash_bytes(bytes: &[u8]) -> u64 {
    BuildHasherDefault::<rustc_hash::FxHasher>::default().hash_one(bytes)
}

/// Encodes a `u32` as a protobuf varint at `dst` and returns the advanced pointer.
///
/// # Safety
/// `dst` must be valid for writes of at least 5 bytes.
#[inline(always)]
unsafe fn encode_u32_varint_ptr(mut v: u32, mut dst: *mut u8) -> *mut u8 {
    unsafe {
        if v < 0x80 {
            dst.write(v as u8);
            return dst.add(1);
        }
        if v < 0x4000 {
            dst.write((v as u8) | 0x80);
            dst.add(1).write((v >> 7) as u8);
            return dst.add(2);
        }
        while v >= 0x80 {
            dst.write((v as u8) | 0x80);
            dst = dst.add(1);
            v >>= 7;
        }
        dst.write(v as u8);
        dst.add(1)
    }
}

/// Appends a `u32` protobuf varint to `out` with fallible capacity reservation.
#[inline]
fn push_u32_varint(mut v: u32, out: &mut Vec<u8>) -> anyhow::Result<()> {
    out.try_reserve(5)?;
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    Ok(())
}

impl StackTraceStore {
    /// Maximum frames encoded on the stack without heap allocation.
    const STACK_FRAMES_LIMIT: usize = 256;

    #[inline]
    fn bytes_at(&self, idx: u32) -> &[u8] {
        let i = idx as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let end = self.ends[i] as usize;
        &self.data[start..end]
    }

    /// Interns a canonical packed-varint stack trace slice by exact byte equality.
    fn intern_packed(&mut self, packed: &[u8], max_loc: u32) -> anyhow::Result<StackTraceId> {
        let hash = hash_bytes(packed);

        // Ensure spare capacity only on a miss when full, so cache hits never grow or OOM,
        // and ordinary misses do a single probe via `entry`.
        if self.table.len() == self.table.capacity() {
            if let Some(&idx) = self.table.find(hash, |&i| self.bytes_at(i) == packed) {
                return Ok(StackTraceId::from_offset(idx as usize));
            }
            let data = &self.data;
            let ends = &self.ends;
            self.table
                .try_reserve(1, |&i| {
                    let i = i as usize;
                    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                    hash_bytes(&data[start..ends[i] as usize])
                })
                .map_err(|e| anyhow::anyhow!("failed to reserve stack trace table: {e}"))?;
        }

        let data = &self.data;
        let ends = &self.ends;
        let entry = self.table.entry(
            hash,
            |&i| {
                let i = i as usize;
                let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                &data[start..ends[i] as usize] == packed
            },
            |&i| {
                let i = i as usize;
                let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                hash_bytes(&data[start..ends[i] as usize])
            },
        );

        match entry {
            Entry::Occupied(occ) => Ok(StackTraceId::from_offset(*occ.get() as usize)),
            Entry::Vacant(vac) => {
                let idx = u32::try_from(self.ends.len())
                    .context("stack trace count exceeded u32::MAX")?;
                let new_end = self
                    .data
                    .len()
                    .checked_add(packed.len())
                    .context("stack trace slab length overflow")?;
                let new_end =
                    u32::try_from(new_end).context("stack trace slab exceeded u32::MAX bytes")?;

                self.data.try_reserve(packed.len())?;
                self.ends.try_reserve(1)?;

                self.data.extend_from_slice(packed);
                self.ends.push(new_end);
                if max_loc > self.max_location_id {
                    self.max_location_id = max_loc;
                }
                vac.insert(idx);
                Ok(StackTraceId::from_offset(idx as usize))
            }
        }
    }

    /// Interns a slice of generational location IDs without heap allocation on cache hits
    /// (for stacks up to `STACK_FRAMES_LIMIT` frames).
    pub fn intern_generational(
        &mut self,
        locations: &[GenerationalId<LocationId>],
        generation: Generation,
    ) -> anyhow::Result<StackTraceId> {
        let mut max_loc: u32 = 0;
        if locations.len() <= Self::STACK_FRAMES_LIMIT {
            let mut buf = [MaybeUninit::<u8>::uninit(); Self::STACK_FRAMES_LIMIT * 5];
            let base = buf.as_mut_ptr().cast::<u8>();
            let mut dst = base;
            for l in locations {
                let id = l.get(generation)?.as_u32();
                if id > max_loc {
                    max_loc = id;
                }
                // SAFETY: `locations.len() <= STACK_FRAMES_LIMIT` and each u32 varint
                // writes at most 5 bytes, so `dst` has >= 5 bytes remaining in `buf`.
                dst = unsafe { encode_u32_varint_ptr(id, dst) };
            }
            // SAFETY: `base..dst` lies within `buf` and every byte in that range was written.
            let packed = unsafe {
                let len = dst.offset_from(base) as usize;
                std::slice::from_raw_parts(base, len)
            };
            return self.intern_packed(packed, max_loc);
        }

        let mut vec = Vec::new();
        vec.try_reserve(locations.len().saturating_mul(2))?;
        for l in locations {
            let id = l.get(generation)?.as_u32();
            if id > max_loc {
                max_loc = id;
            }
            push_u32_varint(id, &mut vec)?;
        }
        self.intern_packed(&vec, max_loc)
    }

    /// Interns a slice of resolved `LocationId`s.
    pub fn intern_slice(&mut self, locations: &[LocationId]) -> anyhow::Result<StackTraceId> {
        let mut max_loc: u32 = 0;
        if locations.len() <= Self::STACK_FRAMES_LIMIT {
            let mut buf = [MaybeUninit::<u8>::uninit(); Self::STACK_FRAMES_LIMIT * 5];
            let base = buf.as_mut_ptr().cast::<u8>();
            let mut dst = base;
            for l in locations {
                let id = l.as_u32();
                if id > max_loc {
                    max_loc = id;
                }
                // SAFETY: `locations.len() <= STACK_FRAMES_LIMIT` and each u32 varint
                // writes at most 5 bytes, so `dst` has >= 5 bytes remaining in `buf`.
                dst = unsafe { encode_u32_varint_ptr(id, dst) };
            }
            // SAFETY: `base..dst` lies within `buf` and every byte in that range was written.
            let packed = unsafe {
                let len = dst.offset_from(base) as usize;
                std::slice::from_raw_parts(base, len)
            };
            return self.intern_packed(packed, max_loc);
        }

        let mut vec = Vec::new();
        vec.try_reserve(locations.len().saturating_mul(2))?;
        for l in locations {
            let id = l.as_u32();
            if id > max_loc {
                max_loc = id;
            }
            push_u32_varint(id, &mut vec)?;
        }
        self.intern_packed(&vec, max_loc)
    }

    /// Returns the canonical packed protobuf varint bytes for `id`.
    #[inline]
    pub fn get_packed(&self, id: StackTraceId) -> anyhow::Result<&[u8]> {
        let i = id.to_raw_id();
        anyhow::ensure!(
            i < self.ends.len(),
            "StackTraceId {id:?} to exist in profile"
        );
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let end = self.ends[i] as usize;
        Ok(&self.data[start..end])
    }

    /// Returns the maximum 1-based `LocationId` across all interned stack traces (0 if none).
    #[inline]
    pub fn max_location_id(&self) -> u32 {
        self.max_location_id
    }
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

    #[test]
    fn stack_trace_store_dedup_and_packed_varints() {
        let mut store = StackTraceStore::default();
        let empty_id = store.intern_slice(&[]).unwrap();
        assert_eq!(store.get_packed(empty_id).unwrap(), &[] as &[u8]);
        assert_eq!(store.intern_slice(&[]).unwrap(), empty_id);
        assert_eq!(store.max_location_id(), 0);

        let locs = vec![
            LocationId::from_offset(0),       // 1 (1 byte)
            LocationId::from_offset(126),     // 127 (1 byte)
            LocationId::from_offset(127),     // 128 (2 bytes)
            LocationId::from_offset(16383),   // 16384 (3 bytes)
            LocationId::from_offset(2097151), // 2097152 (4 bytes)
        ];
        let id1 = store.intern_slice(&locs).unwrap();
        let id2 = store.intern_slice(&locs).unwrap();
        assert_eq!(id1, id2);
        assert_ne!(id1, empty_id);
        assert_eq!(store.max_location_id(), 2097152);

        let mut expected = Vec::new();
        libdd_profiling_protobuf::encode_packed_u64(
            locs.iter().map(LocationId::to_raw_id),
            &mut expected,
        )
        .unwrap();
        assert_eq!(store.get_packed(id1).unwrap(), expected.as_slice());

        // > STACK_FRAMES_LIMIT fallback path
        let deep: Vec<LocationId> = (0..300).map(LocationId::from_offset).collect();
        let deep_id1 = store.intern_slice(&deep).unwrap();
        let deep_id2 = store.intern_slice(&deep).unwrap();
        assert_eq!(deep_id1, deep_id2);
        let mut expected_deep = Vec::new();
        libdd_profiling_protobuf::encode_packed_u64(
            deep.iter().map(LocationId::to_raw_id),
            &mut expected_deep,
        )
        .unwrap();
        assert_eq!(
            store.get_packed(deep_id1).unwrap(),
            expected_deep.as_slice()
        );
    }
}
