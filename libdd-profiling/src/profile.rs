// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The profile: samples aggregated by stack, labels and trace context, kept
//! in one format-neutral model that the encoders lower to pprof or OTLP.
//!
//! # Locking
//!
//! A [`Profile`] is not locked: every mutation takes `&mut self`. A caller
//! sharing one between threads wraps it in its own mutex.
//!
//! What the profile refers to is pluggable, through traits, so each runtime
//! picks its own granularity of sharing and locking:
//!
//! - Strings and functions are read through [`Resolve`]. An [`InternTable`] is shared between
//!   threads and processes and locks internally; any other table works too.
//! - Stacks are stored through [`Stacks`]. [`ProfileStacks`] keeps them in the profile itself,
//!   deduplicated per profile and cleared on reset, so samples pass their frames. A runtime that
//!   interns whole stacks itself implements [`Stacks`] over its own table and passes stack ids
//!   instead.
//!
//! # Trace context
//!
//! Every sample may carry an OpenTelemetry [`Link`] (trace id and span id)
//! and a local root span id. The profile keeps all three; each encoder emits
//! only what its format uses. pprof has no trace id, and OTLP has no local
//! root span id.

use crate::collections::alloc::Global;
use crate::collections::index_map::IndexMap;
use crate::collections::index_set::IndexSet;
use crate::collections::vec::TryVec;
use crate::intern_table::{FunctionId, Id, InternTable, Record, StringId};
use core::fmt;
use core::hash::Hash;

pub use crate::intern_table::{Function, Str};

/// Read access to interned values of `R`.
pub trait Resolve<R: Record> {
    /// The value with id `id`, if there is one.
    fn resolve(&self, id: Id<R>) -> Option<R::Key<'_>>;
}

impl<R: Record> Resolve<R> for InternTable<R> {
    fn resolve(&self, id: Id<R>) -> Option<R::Key<'_>> {
        Self::resolve(self, id)
    }
}

impl<R: Record, T: Resolve<R> + ?Sized> Resolve<R> for &T {
    fn resolve(&self, id: Id<R>) -> Option<R::Key<'_>> {
        T::resolve(self, id)
    }
}

/// One of a profile's value columns, such as `cpu-time` in `nanoseconds`.
/// Which columns a format carries, and under what names, is decided when
/// the profile is encoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleType {
    pub name: StringId,
    pub unit: StringId,
}

/// A profile's sample type: its position in the types the profile was
/// created with. Any value can be made; one the profile does not have is
/// rejected where it is used.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SampleTypeId(u32);

impl SampleTypeId {
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A sample's value for one sample type. Types a sample leaves out count
/// as 0.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleValue {
    pub sample_type: SampleTypeId,
    pub value: i64,
}

/// An OpenTelemetry link to the span a sample was taken in. All zeros means
/// no span.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Link {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
}

impl Link {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The span id as a `u64`, the form Datadog uses.
    #[must_use]
    pub const fn span_id_u64(&self) -> u64 {
        u64::from_be_bytes(self.span_id)
    }
}

/// A label's value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LabelValue {
    /// A string from the shared string table.
    Str(StringId),
    /// A string kept by this profile only ([`Profile::intern_local`]), for
    /// high-cardinality values such as task names: it is freed on reset
    /// instead of living in the shared table forever.
    Local(u32),
    /// A number, with an optional unit (`StringId::ZERO` for none).
    Num { value: i64, unit: StringId },
}

/// A key-value label on a sample, such as `thread name`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Label {
    pub key: StringId,
    pub value: LabelValue,
}

/// One frame of a stack: a function and a position in it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Frame {
    pub address: u64,
    pub line: i64,
    pub function: FunctionId,
}

/// A sample to add.
#[derive(Clone, Copy, Debug)]
pub struct Sample<'a, I> {
    /// The stack, leaf first, in the form the profile's [`Stacks`] takes.
    pub stack: I,
    /// The values this sample measured, each sample type at most once.
    pub values: &'a [SampleValue],
    pub labels: &'a [Label],
    pub link: Link,
    /// 0 means none.
    pub local_root_span_id: u64,
    /// Unix nanoseconds; 0 aggregates the sample with others like it instead
    /// of keeping it as its own event.
    pub timestamp_ns: i64,
}

/// Why a profile operation failed. The profile is unchanged.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    OutOfMemory,
    /// The sample's values do not match the profile's sample types, or an
    /// argument is out of range.
    InvalidArgument,
    /// Too many distinct stacks, label sets or links for `u32` indices.
    TooMany,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfMemory => "out of memory",
            Self::InvalidArgument => "invalid argument",
            Self::TooMany => "too many distinct stacks, label sets or links",
        })
    }
}

impl core::error::Error for ProfileError {}

/// Where a profile keeps its samples' stacks.
pub trait Stacks {
    /// What a sample passes as its stack.
    type Input<'a>: Copy;

    /// Stores `stack` (or looks it up) and returns its id.
    ///
    /// # Errors
    ///
    /// See [`ProfileError`].
    fn add(&mut self, stack: Self::Input<'_>) -> Result<u32, ProfileError>;

    /// The frames of a stored stack, leaf first.
    fn frames(&self, id: u32) -> Option<&[Frame]>;

    /// Forgets every stack, when the profile is reset.
    fn clear(&mut self);
}

/// Stacks kept in the profile, deduplicated per profile. Samples pass their
/// frames.
pub struct ProfileStacks {
    stacks: IndexSet<TryVec<Frame, Global>, Global>,
}

impl ProfileStacks {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stacks: IndexSet::new_in(Global),
        }
    }

    /// Distinct stacks stored.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.stacks.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stacks.is_empty()
    }
}

impl Default for ProfileStacks {
    fn default() -> Self {
        Self::new()
    }
}

impl Stacks for ProfileStacks {
    type Input<'a> = &'a [Frame];

    fn add(&mut self, frames: &[Frame]) -> Result<u32, ProfileError> {
        let index = if let Some(index) = self.stacks.get_index_of(frames) {
            index
        } else {
            let mut owned = TryVec::try_with_capacity_in(frames.len(), Global)
                .map_err(|_| ProfileError::OutOfMemory)?;
            owned
                .try_extend_from_slice(frames)
                .map_err(|_| ProfileError::OutOfMemory)?;
            self.stacks
                .try_insert(owned)
                .map_err(|_| ProfileError::OutOfMemory)?
                .0
        };
        u32::try_from(index).map_err(|_| ProfileError::TooMany)
    }

    fn frames(&self, key: u32) -> Option<&[Frame]> {
        let index = usize::try_from(key).ok()?;
        self.stacks.get_index(index).map(TryVec::as_slice)
    }

    fn clear(&mut self) {
        self.stacks = IndexSet::new_in(Global);
    }
}

/// What identifies a sample, timed or not; untimed samples with equal keys
/// are aggregated. 20 bytes with 4-byte alignment, so it packs with `u32`s.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SampleKey {
    /// The local root span id in native byte order (`u64::to_ne_bytes`),
    /// so the key needs only 4-byte alignment. 0 means none.
    local_root_span_id: [u8; 8],
    /// Id from the profile's [`Stacks`].
    pub stack: u32,
    /// Index into the profile's label sets.
    pub labels: u32,
    /// Index into the profile's links; 0 is the empty link.
    pub link: u32,
}

impl SampleKey {
    /// The local root span id; 0 means none.
    #[must_use]
    pub const fn local_root_span_id(&self) -> u64 {
        u64::from_ne_bytes(self.local_root_span_id)
    }
}

/// A timestamped sample. Its values are `timed_values[start..start + N]`
/// for N sample types. Laid out to be 32 bytes with no padding.
#[derive(Clone, Copy, Debug)]
struct TimedSample {
    timestamp_ns: i64,
    key: SampleKey,
    start: u32,
}

const _: () = assert!(size_of::<SampleKey>() == 20 && align_of::<SampleKey>() == 4);
const _: () = assert!(size_of::<TimedSample>() == 32);

/// Appends a row of `width` values to `values`, whose length is `start`, with
/// `sample`'s values in their columns and 0 elsewhere. Capacity for the row
/// must be reserved; `sample`'s types must be under `width`.
fn push_row(values: &mut TryVec<i64, Global>, start: usize, width: usize, sample: &[SampleValue]) {
    for _ in 0..width {
        let _ = values.push_within_capacity(0);
    }
    for v in sample {
        if let Some(slot) = usize::try_from(v.sample_type.index())
            .ok()
            .and_then(|t| start.checked_add(t))
            .and_then(|at| values.get_mut(at))
        {
            *slot = v.value;
        }
    }
}

/// Where the next sample's values start, if `len` values are stored already
/// and the sample adds `width` more: every index must fit a `u32`.
fn values_start(len: usize, width: usize) -> Option<u32> {
    let end = len.checked_add(width)?;
    u32::try_from(end).ok()?;
    u32::try_from(len).ok()
}

/// A profile under construction. See the [module documentation](self).
pub struct Profile<S, F, K: Stacks> {
    strings: S,
    functions: F,
    stacks: K,
    sample_types: TryVec<SampleType, Global>,
    start_ns: i64,
    label_sets: IndexSet<TryVec<Label, Global>, Global>,
    links: IndexSet<Link, Global>,
    /// Aggregated samples: each key's N values are `values[start..start + N]`.
    aggregated: IndexMap<SampleKey, u32, Global>,
    values: TryVec<i64, Global>,
    /// Timestamped samples, in the order they were added, with their values
    /// in `timed_values`.
    // TODO: an option to compress timed samples in memory, as v1 did; runtimes
    // with truly parallel threads collect enough of them to benefit.
    timed: TryVec<TimedSample, Global>,
    timed_values: TryVec<i64, Global>,
    /// Profile-local label strings, freed on reset.
    local_strings: IndexSet<TryVec<u8, Global>, Global>,
    /// Local root span id to trace endpoint (resource name).
    /// Values are profile-local string ids.
    endpoints: IndexMap<u64, u32, Global>,
    endpoint_counts: IndexMap<u32, i64, Global>,
}

impl<S, F, K> Profile<S, F, K>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    /// A new profile with the given sample types, starting at `start_ns`
    /// (Unix nanoseconds).
    ///
    /// # Errors
    ///
    /// [`ProfileError::InvalidArgument`] if there are no sample types or
    /// more than `u32::MAX`, and [`ProfileError::OutOfMemory`].
    pub fn try_new(
        strings: S,
        functions: F,
        stacks: K,
        sample_types: &[SampleType],
        start_ns: i64,
    ) -> Result<Self, ProfileError> {
        if sample_types.is_empty() || u32::try_from(sample_types.len()).is_err() {
            return Err(ProfileError::InvalidArgument);
        }
        let mut types = TryVec::try_with_capacity_in(sample_types.len(), Global)
            .map_err(|_| ProfileError::OutOfMemory)?;
        types
            .try_extend_from_slice(sample_types)
            .map_err(|_| ProfileError::OutOfMemory)?;
        let mut profile = Self {
            strings,
            functions,
            stacks,
            sample_types: types,
            start_ns,
            label_sets: IndexSet::new_in(Global),
            links: IndexSet::new_in(Global),
            aggregated: IndexMap::new_in(Global),
            values: TryVec::new_in(Global),
            timed: TryVec::new_in(Global),
            timed_values: TryVec::new_in(Global),
            local_strings: IndexSet::new_in(Global),
            endpoints: IndexMap::new_in(Global),
            endpoint_counts: IndexMap::new_in(Global),
        };
        profile.reserve_zero_entries()?;
        Ok(profile)
    }

    /// Index 0 of label sets and links is the empty one.
    fn reserve_zero_entries(&mut self) -> Result<(), ProfileError> {
        self.label_sets
            .try_insert(TryVec::new_in(Global))
            .map_err(|_| ProfileError::OutOfMemory)?;
        self.links
            .try_insert(Link::default())
            .map_err(|_| ProfileError::OutOfMemory)?;
        Ok(())
    }

    /// Adds a sample: aggregated with like samples, or kept as its own event
    /// when it has a timestamp.
    ///
    /// # Errors
    ///
    /// [`ProfileError::InvalidArgument`] if a value names a sample type the
    /// profile does not have, or (with debug assertions) names one twice;
    /// otherwise see [`ProfileError`]. Naming a type twice is a caller bug:
    /// release builds skip that check, and the sample's value for the type is
    /// then one of the duplicates or their sum.
    pub fn add_sample(&mut self, sample: Sample<'_, K::Input<'_>>) -> Result<(), ProfileError> {
        let width = self.sample_types.len();
        let in_range = sample
            .values
            .iter()
            .all(|v| usize::try_from(v.sample_type.index()).is_ok_and(|t| t < width));
        if !in_range {
            return Err(ProfileError::InvalidArgument);
        }
        // Quadratic, but samples carry a handful of values.
        #[cfg(debug_assertions)]
        if sample.values.iter().enumerate().any(|(i, v)| {
            sample
                .values
                .iter()
                .take(i)
                .any(|earlier| earlier.sample_type == v.sample_type)
        }) {
            return Err(ProfileError::InvalidArgument);
        }
        let key = SampleKey {
            local_root_span_id: sample.local_root_span_id.to_ne_bytes(),
            stack: self.stacks.add(sample.stack)?,
            labels: self.label_set(sample.labels)?,
            link: self.link(sample.link)?,
        };
        if sample.timestamp_ns != 0 {
            let start =
                values_start(self.timed_values.len(), width).ok_or(ProfileError::TooMany)?;
            self.timed
                .try_reserve(1)
                .and_then(|()| self.timed_values.try_reserve(width))
                .map_err(|_| ProfileError::OutOfMemory)?;
            let _ = self.timed.push_within_capacity(TimedSample {
                timestamp_ns: sample.timestamp_ns,
                key,
                start,
            });
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            push_row(&mut self.timed_values, start, width, sample.values);
            return Ok(());
        }
        if let Some(&start) = self.aggregated.get(&key) {
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            if let Some(row) = self.values.get_mut(start..start.saturating_add(width)) {
                for v in sample.values {
                    if let Some(slot) = usize::try_from(v.sample_type.index())
                        .ok()
                        .and_then(|t| row.get_mut(t))
                    {
                        *slot = slot.saturating_add(v.value);
                    }
                }
            }
            return Ok(());
        }
        let start = values_start(self.values.len(), width).ok_or(ProfileError::TooMany)?;
        self.values
            .try_reserve(width)
            .map_err(|_| ProfileError::OutOfMemory)?;
        self.aggregated
            .try_insert(key, start)
            .map_err(|_| ProfileError::OutOfMemory)?;
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        push_row(&mut self.values, start, width, sample.values);
        Ok(())
    }

    fn label_set(&mut self, labels: &[Label]) -> Result<u32, ProfileError> {
        let index = if let Some(index) = self.label_sets.get_index_of(labels) {
            index
        } else {
            let mut owned = TryVec::try_with_capacity_in(labels.len(), Global)
                .map_err(|_| ProfileError::OutOfMemory)?;
            owned
                .try_extend_from_slice(labels)
                .map_err(|_| ProfileError::OutOfMemory)?;
            self.label_sets
                .try_insert(owned)
                .map_err(|_| ProfileError::OutOfMemory)?
                .0
        };
        u32::try_from(index).map_err(|_| ProfileError::TooMany)
    }

    fn link(&mut self, link: Link) -> Result<u32, ProfileError> {
        let (index, _) = self
            .links
            .try_insert(link)
            .map_err(|_| ProfileError::OutOfMemory)?;
        u32::try_from(index).map_err(|_| ProfileError::TooMany)
    }

    /// Keeps `s` in this profile until the next reset, for a
    /// [`LabelValue::Local`] label.
    ///
    /// # Errors
    ///
    /// [`ProfileError::OutOfMemory`], or [`ProfileError::TooMany`] past
    /// `u32` distinct strings.
    pub fn intern_local(&mut self, s: &str) -> Result<u32, ProfileError> {
        let index = if let Some(index) = self.local_strings.get_index_of(s.as_bytes()) {
            index
        } else {
            let mut owned = TryVec::try_with_capacity_in(s.len(), Global)
                .map_err(|_| ProfileError::OutOfMemory)?;
            owned
                .try_extend_from_slice(s.as_bytes())
                .map_err(|_| ProfileError::OutOfMemory)?;
            self.local_strings
                .try_insert(owned)
                .map_err(|_| ProfileError::OutOfMemory)?
                .0
        };
        u32::try_from(index).map_err(|_| ProfileError::TooMany)
    }

    /// A profile-local string; empty for an unknown id.
    pub fn local_string(&self, id: u32) -> &str {
        usize::try_from(id)
            .ok()
            .and_then(|index| self.local_strings.get_index(index))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }

    /// Records the trace endpoint (the root span's resource) of a local root
    /// span, for samples that carry its id. Endpoints are kept by this profile
    /// only (like [`LabelValue::Local`] strings), not in the shared table.
    ///
    /// # Errors
    ///
    /// [`ProfileError::OutOfMemory`] or [`ProfileError::TooMany`].
    pub fn set_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: &str,
    ) -> Result<(), ProfileError> {
        let endpoint = self.intern_local(endpoint)?;
        self.endpoints
            .try_insert(local_root_span_id, endpoint)
            .map(|_| ())
            .map_err(|_| ProfileError::OutOfMemory)
    }

    /// Adds `count` hits of a trace endpoint.
    ///
    /// # Errors
    ///
    /// [`ProfileError::OutOfMemory`] or [`ProfileError::TooMany`].
    pub fn add_endpoint_count(&mut self, endpoint: &str, count: i64) -> Result<(), ProfileError> {
        let endpoint = self.intern_local(endpoint)?;
        let previous = self.endpoint_counts.get(&endpoint).copied().unwrap_or(0);
        self.endpoint_counts
            .try_insert(endpoint, previous.saturating_add(count))
            .map(|_| ())
            .map_err(|_| ProfileError::OutOfMemory)
    }

    /// Drops every sample and endpoint, keeping the configuration, and
    /// starts a new period at `start_ns`.
    ///
    /// # Errors
    ///
    /// [`ProfileError::OutOfMemory`] if the empty label set or link cannot
    /// be re-added; the profile is then empty but unusable until a reset
    /// succeeds.
    pub fn reset(&mut self, start_ns: i64) -> Result<(), ProfileError> {
        self.stacks.clear();
        self.label_sets = IndexSet::new_in(Global);
        self.links = IndexSet::new_in(Global);
        self.aggregated = IndexMap::new_in(Global);
        self.values.clear();
        self.timed.clear();
        self.timed_values.clear();
        self.local_strings = IndexSet::new_in(Global);
        self.endpoints = IndexMap::new_in(Global);
        self.endpoint_counts = IndexMap::new_in(Global);
        self.start_ns = start_ns;
        self.reserve_zero_entries()
    }

    // Read access for the encoders.

    pub const fn strings(&self) -> &S {
        &self.strings
    }

    pub const fn functions(&self) -> &F {
        &self.functions
    }

    pub const fn stacks(&self) -> &K {
        &self.stacks
    }

    pub fn sample_types(&self) -> &[SampleType] {
        &self.sample_types
    }

    /// The id of the sample type at `index`, if the profile has one.
    #[must_use]
    pub fn sample_type_id(&self, index: usize) -> Option<SampleTypeId> {
        (index < self.sample_types.len())
            .then(|| u32::try_from(index).ok().map(SampleTypeId))
            .flatten()
    }

    /// The sample type with `id`, if the profile has one.
    #[must_use]
    pub fn sample_type(&self, id: SampleTypeId) -> Option<&SampleType> {
        self.sample_types.get(usize::try_from(id.index()).ok()?)
    }

    pub const fn start_ns(&self) -> i64 {
        self.start_ns
    }

    /// Every sample: aggregated ones (timestamp 0) first, then timed ones in
    /// the order they were added. Values have one entry per sample type.
    pub fn samples(&self) -> impl Iterator<Item = (SampleKey, &[i64], i64)> + '_ {
        let width = self.sample_types.len();
        let aggregated = self.aggregated.iter().filter_map(move |(key, &start)| {
            let start = usize::try_from(start).ok()?;
            let values = self.values.get(start..start.checked_add(width)?)?;
            Some((*key, values, 0))
        });
        let timed = self.timed.iter().filter_map(move |sample| {
            let start = usize::try_from(sample.start).ok()?;
            let values = self.timed_values.get(start..start.checked_add(width)?)?;
            Some((sample.key, values, sample.timestamp_ns))
        });
        aggregated.chain(timed)
    }

    /// The labels of a label set.
    pub fn labels(&self, index: u32) -> &[Label] {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.label_sets.get_index(index))
            .map_or(&[], |labels| labels.as_slice())
    }

    /// The link with an index; index 0 is the empty link.
    pub fn link_at(&self, index: u32) -> Link {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.links.get_index(index))
            .map_or_else(Link::default, |link| *link)
    }

    /// The trace endpoint of a local root span, if one was set.
    pub fn endpoint(&self, local_root_span_id: u64) -> Option<&str> {
        if local_root_span_id == 0 {
            return None;
        }
        let id = self.endpoints.get(&local_root_span_id).copied()?;
        Some(self.local_string(id))
    }

    /// Trace endpoint hit counts.
    pub fn endpoint_counts(&self) -> impl Iterator<Item = (&str, i64)> + '_ {
        self.endpoint_counts
            .iter()
            .map(|(&e, &c)| (self.local_string(e), c))
    }
}

impl<S, F, K: Stacks> fmt::Debug for Profile<S, F, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("sample_types", &self.sample_types.len())
            .field("aggregated", &self.aggregated.len())
            .field("timed", &self.timed.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::values_start;

    #[test]
    fn timed_values_must_be_indexable_by_u32() {
        assert_eq!(values_start(0, 4), Some(0));
        let max = usize::try_from(u32::MAX).unwrap();
        assert_eq!(values_start(max - 4, 4), Some(u32::MAX - 4));
        assert_eq!(values_start(max - 3, 4), None);
        assert_eq!(values_start(usize::MAX, 1), None);
    }
}
