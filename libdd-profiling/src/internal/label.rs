// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::internal::interning_api::{Generation, GenerationalId};
use anyhow::Context;
use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use libdd_profiling_protobuf::{Record, StringOffset, prost_impls};
use std::hash::{BuildHasher, BuildHasherDefault};
use std::mem::MaybeUninit;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum LabelValue {
    Str(StringId),
    Num { num: i64, num_unit: StringId },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Label {
    key: StringId,
    value: LabelValue,
}

impl Label {
    pub fn has_num_value(&self) -> bool {
        matches!(self.value, LabelValue::Num { .. })
    }

    pub fn has_string_value(&self) -> bool {
        matches!(self.value, LabelValue::Str(_))
    }

    pub fn get_key(&self) -> StringId {
        self.key
    }

    pub fn get_value(&self) -> &LabelValue {
        &self.value
    }

    pub fn num(key: StringId, num: i64, num_unit: StringId) -> Self {
        Self {
            key,
            value: LabelValue::Num { num, num_unit },
        }
    }

    pub fn str(key: StringId, v: StringId) -> Self {
        Self {
            key,
            value: LabelValue::Str(v),
        }
    }
}

impl From<Label> for prost_impls::Label {
    fn from(l: Label) -> Self {
        Self::from(&l)
    }
}

impl From<&Label> for prost_impls::Label {
    fn from(l: &Label) -> prost_impls::Label {
        let key = l.key.to_raw_id();
        match l.value {
            LabelValue::Str(str) => Self {
                key,
                str: str.to_raw_id(),
                num: 0,
                num_unit: 0,
            },
            LabelValue::Num { num, num_unit } => Self {
                key,
                str: 0,
                num,
                num_unit: num_unit.into_raw_id(),
            },
        }
    }
}

impl From<Label> for libdd_profiling_protobuf::Label {
    fn from(label: Label) -> Self {
        Self::from(&label)
    }
}

impl From<&Label> for libdd_profiling_protobuf::Label {
    fn from(label: &Label) -> Self {
        let (str, num, num_unit) = match label.value {
            LabelValue::Str(str) => (str, 0, StringOffset::ZERO),
            LabelValue::Num { num, num_unit } => (StringOffset::ZERO, num, num_unit),
        };
        Self {
            key: Record::from(label.key),
            str: Record::from(str),
            num: Record::from(num),
            num_unit: Record::from(num_unit),
        }
    }
}

impl Item for Label {
    type Id = LabelId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(C)]
pub struct LabelId(u32);

impl Id for LabelId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        let index: u32 = inner.try_into().expect("LabelId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}
impl LabelId {
    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}

/// A canonical representation for sets of labels.
/// You should only use the impl functions to modify this.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LabelSet {
    labels: Box<[LabelId]>,
}

/// Compact contiguous storage for deduplicated label sets.
///
/// Stores all `LabelId`s in a single flat `Vec<LabelId>` (`data`) indexed by end offsets
/// (`ends`) and deduplicated by exact slice equality via `table`.
#[derive(Default)]
pub struct LabelSetStore {
    data: Vec<LabelId>,
    ends: Vec<u32>,
    table: HashTable<u32>,
}

#[inline]
fn hash_label_ids(labels: &[LabelId]) -> u64 {
    BuildHasherDefault::<rustc_hash::FxHasher>::default().hash_one(labels)
}

impl LabelSetStore {
    const STACK_LABELS_LIMIT: usize = 32;

    #[inline]
    fn slice_at(&self, idx: u32) -> &[LabelId] {
        let i = idx as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let end = self.ends[i] as usize;
        &self.data[start..end]
    }

    pub fn intern_slice(&mut self, labels: &[LabelId]) -> anyhow::Result<LabelSetId> {
        let hash = hash_label_ids(labels);

        if self.table.len() == self.table.capacity() {
            if let Some(&idx) = self.table.find(hash, |&i| self.slice_at(i) == labels) {
                return Ok(LabelSetId::from_offset(idx as usize));
            }
            let data = &self.data;
            let ends = &self.ends;
            self.table
                .try_reserve(1, |&i| {
                    let i = i as usize;
                    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                    hash_label_ids(&data[start..ends[i] as usize])
                })
                .map_err(|e| anyhow::anyhow!("failed to reserve label set table: {e}"))?;
        }

        let data = &self.data;
        let ends = &self.ends;
        let entry = self.table.entry(
            hash,
            |&i| {
                let i = i as usize;
                let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                &data[start..ends[i] as usize] == labels
            },
            |&i| {
                let i = i as usize;
                let start = if i == 0 { 0 } else { ends[i - 1] as usize };
                hash_label_ids(&data[start..ends[i] as usize])
            },
        );

        match entry {
            Entry::Occupied(occ) => Ok(LabelSetId::from_offset(*occ.get() as usize)),
            Entry::Vacant(vac) => {
                let idx =
                    u32::try_from(self.ends.len()).context("label set count exceeded u32::MAX")?;
                let new_end = self
                    .data
                    .len()
                    .checked_add(labels.len())
                    .context("label set slab length overflow")?;
                let new_end =
                    u32::try_from(new_end).context("label set slab exceeded u32::MAX entries")?;

                self.data.try_reserve(labels.len())?;
                self.ends.try_reserve(1)?;

                self.data.extend_from_slice(labels);
                self.ends.push(new_end);
                vac.insert(idx);
                Ok(LabelSetId::from_offset(idx as usize))
            }
        }
    }

    pub fn intern_generational(
        &mut self,
        labels: &[GenerationalId<LabelId>],
        generation: Generation,
    ) -> anyhow::Result<LabelSetId> {
        if labels.len() <= Self::STACK_LABELS_LIMIT {
            let mut buf = [MaybeUninit::<LabelId>::uninit(); Self::STACK_LABELS_LIMIT];
            for (slot, l) in buf.iter_mut().zip(labels.iter()) {
                slot.write(l.get(generation)?);
            }
            // SAFETY: The first `labels.len()` elements were initialized in the loop above.
            let slice =
                unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<LabelId>(), labels.len()) };
            return self.intern_slice(slice);
        }

        let mut vec = Vec::new();
        vec.try_reserve_exact(labels.len())?;
        for l in labels {
            vec.push(l.get(generation)?);
        }
        self.intern_slice(&vec)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.data.clear();
        self.ends.clear();
        self.table.clear();
    }

    #[inline]
    #[allow(dead_code)]
    pub fn get(&self, id: LabelSetId) -> Option<&[LabelId]> {
        let i = id.to_offset();
        if i >= self.ends.len() {
            return None;
        }
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let end = self.ends[i] as usize;
        Some(&self.data[start..end])
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[LabelId]> {
        (0..self.ends.len()).map(|i| {
            let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
            let end = self.ends[i] as usize;
            &self.data[start..end]
        })
    }
}

impl LabelSet {
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn iter(&self) -> core::slice::Iter<'_, LabelId> {
        self.labels.iter()
    }

    pub fn labels(&self) -> &[LabelId] {
        &self.labels
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn new(labels: Box<[LabelId]>) -> Self {
        // Once upon a time label ids were guaranteed to be sorted. However,
        // this makes testing difficult because the order of input labels and
        // output labels can make a difference.
        // Unless there is some reason lost to time, we do not need to sort
        // these. Save some cycles, and if a given language increases memory,
        // then it means they aren't adding labels in the same order every
        // time, and they should examine that--but it shouldn't be a
        // correctness issue, as far as I know.
        Self { labels }
    }
}

impl Item for LabelSet {
    type Id = LabelSetId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(C)]
#[cfg_attr(test, derive(bolero::generator::TypeGenerator))]
pub struct LabelSetId(u32);

impl Id for LabelSetId {
    type RawId = usize;

    fn from_offset(inner: usize) -> Self {
        #[allow(clippy::expect_used)]
        let index: u32 = inner.try_into().expect("LabelSetId to fit into a u32");
        Self(index)
    }

    fn to_raw_id(&self) -> Self::RawId {
        self.0 as Self::RawId
    }
}

impl LabelSetId {
    #[inline]
    pub fn to_offset(&self) -> usize {
        self.0 as usize
    }
}

impl From<LabelSetId> for u32 {
    fn from(value: LabelSetId) -> Self {
        value.0
    }
}
