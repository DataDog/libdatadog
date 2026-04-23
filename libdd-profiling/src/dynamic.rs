// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;
use crate::collections::identifiable::{FxIndexMap, FxIndexSet, StringId};
use crate::collections::string_table::{self, StringTable};
use crate::internal::{EncodedProfile, Profile as NativeProfile, ProfiledEndpointsStats};
use crate::profiles::{DefaultObservationCodec as DefaultCodec, ObservationCodec};
use indexmap::map::{raw_entry_v1::RawEntryMut, RawEntryApiV1};
use libdd_alloc::{Allocator, ChainAllocator, VirtualAllocator};
use libdd_profiling_protobuf::prost_impls::{self as pprof, Message};
use smallvec::SmallVec;
use std::alloc::Layout;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault};
use std::io::{self, BufWriter, Read, Write};
use std::slice;
use std::time::{Duration, SystemTime};
use thiserror::Error;

type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<rustc_hash::FxHasher>>;
type FxHashSet<V> = HashSet<V, BuildHasherDefault<rustc_hash::FxHasher>>;

const MAX_DYNAMIC_STRING_LENGTH: usize = (1 << 14) - 2;
const MAX_DYNAMIC_FUNCTION_INDEX: usize = 1 << 21;
const PACKED_FUNCTION_BITS: usize = 21;
const PACKED_FUNCTIONS_PER_WORD: usize = 3;
const PACKED_FUNCTION_MASK: u64 = (1 << PACKED_FUNCTION_BITS) - 1;

#[derive(Debug, Error)]
pub enum DynamicProfileError {
    #[error("string was too long for DynamicProfile storage")]
    StringTooLong,
    #[error("dynamic string table was full")]
    StringTableFull,
    #[error("dynamic function table was full")]
    FunctionTableFull,
    #[error("dynamic label set table was full")]
    LabelSetTableFull,
    #[error("dynamic stacktrace table was full")]
    StackTraceTableFull,
    #[error("timestamped observation storage was full")]
    TimestampedObservationStorageFull,
    #[error("timestamped observation stream failed: {0}")]
    TimestampedObservationIo(#[source] io::Error),
    #[error("sample values length mismatch: expected {expected}, got {actual}")]
    ValuesLengthMismatch { expected: usize, actual: usize },
    #[error("label keys must be unique within a sample")]
    DuplicateLabelKey,
    #[error("labels must use at most one of `str` and `num`")]
    InvalidLabelValue,
    #[error("dynamic string index {index} was invalid for this profile")]
    InvalidStringIndex { index: u32 },
    #[error("dynamic function index {index} was invalid for this profile")]
    InvalidFunctionIndex { index: u32 },
    #[error("dynamic stacktrace index {index} was invalid for this profile")]
    InvalidStackTraceIndex { index: u32 },
    #[error("dynamic label set index {index} was invalid for this profile")]
    InvalidLabelSetIndex { index: u32 },
    #[error("dynamic locations must not reference the default function sentinel")]
    EmptyFunctionInLocation,
    #[error("timestamp delta {value} was out of range for i32 storage")]
    TimestampDeltaOutOfRange { value: i64 },
    #[error("upscaling offset {offset} was out of range for {max} sample types")]
    InvalidUpscalingOffset { offset: usize, max: usize },
    #[error("failed to encode profile: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("failed to compress profile: {0}")]
    Compression(#[source] io::Error),
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicStringIndex {
    pub value: u32,
}

impl DynamicStringIndex {
    pub const EMPTY: Self = Self { value: 0 };

    pub const fn is_empty(self) -> bool {
        self.value == 0
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicFunctionIndex {
    pub value: u32,
}

impl DynamicFunctionIndex {
    pub const EMPTY: Self = Self { value: 0 };

    pub const fn is_empty(self) -> bool {
        self.value == 0
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DynamicStackTraceIndex {
    pub value: u32,
}

impl DynamicStackTraceIndex {
    pub const EMPTY: Self = Self { value: 0 };

    pub const fn is_empty(self) -> bool {
        self.value == 0
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DynamicLocation {
    pub function: DynamicFunctionIndex,
    pub line: u32,
}

const _: () = {
    assert!(core::mem::size_of::<DynamicFunctionIndex>() == 4);
    assert!(core::mem::align_of::<DynamicFunctionIndex>() == 4);
    assert!(core::mem::size_of::<DynamicLocation>() == 8);
    assert!(core::mem::align_of::<DynamicLocation>() == 4);
};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DynamicLabel<'a> {
    pub key: DynamicStringIndex,
    pub str: &'a str,
    pub num: i64,
}

impl DynamicLabel<'_> {
    pub fn uses_at_most_one_of_str_and_num(&self) -> bool {
        self.str.is_empty() || self.num == 0
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DynamicSample<'a> {
    pub values: &'a [i64],
    pub labels: &'a [DynamicLabel<'a>],
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
struct StoredFunction {
    name: u32,
    filename: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
struct StoredLabel {
    key: u32,
    str: u32,
    num: i64,
}

const _: () = {
    assert!(core::mem::size_of::<StoredLabel>() == 16);
    assert!(core::mem::align_of::<StoredLabel>() == 8);
};

#[derive(Copy, Clone, Debug)]
struct StoredLabelSlice<'a>(&'a [StoredLabel]);

impl StoredLabelSlice<'_> {
    fn as_bytes(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), core::mem::size_of_val(self.0))
        }
    }
}

impl PartialEq for StoredLabelSlice<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for StoredLabelSlice<'_> {}

impl std::hash::Hash for StoredLabelSlice<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write(self.as_bytes());
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct SampleKey {
    stacktrace: u32,
    labels: u32,
}

#[derive(Default)]
struct DynamicStringTable {
    strings: StringTable,
}

impl DynamicStringTable {
    fn new() -> Self {
        Self {
            strings: StringTable::new(),
        }
    }

    fn len(&self) -> usize {
        self.strings.len()
    }

    fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.strings.iter()
    }

    fn get(&self, index: u32) -> Option<&str> {
        self.strings.get(StringId::from(index))
    }

    fn id_for(&self, value: &str) -> Option<u32> {
        self.strings.get_id(value).map(u32::from)
    }

    fn intern(&mut self, s: &str) -> Result<u32, DynamicProfileError> {
        if s.len() > MAX_DYNAMIC_STRING_LENGTH {
            return Err(DynamicProfileError::StringTooLong);
        }
        if self.strings.len() > u32::MAX as usize {
            return Err(DynamicProfileError::StringTableFull);
        }
        self.strings
            .try_intern(s)
            .map(u32::from)
            .map_err(|err| match err {
                string_table::Error::OutOfMemory | string_table::Error::StorageFull => {
                    DynamicProfileError::StringTableFull
                }
            })
    }
}

struct DynamicFunctionTable {
    entries: FxIndexSet<StoredFunction>,
}

impl DynamicFunctionTable {
    fn new() -> Self {
        let mut entries = FxIndexSet::default();
        entries.reserve(28);
        entries.insert(StoredFunction::default());
        Self { entries }
    }

    fn clear_all(&mut self) {
        *self = Self::new();
    }

    fn get(&self, index: DynamicFunctionIndex) -> Option<&StoredFunction> {
        self.entries.get_index(index.value as usize)
    }

    fn iter_non_empty(&self) -> impl Iterator<Item = (u32, &StoredFunction)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .map(|(offset, item)| (offset as u32, item))
    }

    fn intern(
        &mut self,
        name: DynamicStringIndex,
        filename: DynamicStringIndex,
    ) -> Result<DynamicFunctionIndex, DynamicProfileError> {
        let entry = StoredFunction {
            name: name.value,
            filename: filename.value,
        };
        if self.entries.len() >= MAX_DYNAMIC_FUNCTION_INDEX {
            return Err(DynamicProfileError::FunctionTableFull);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::FunctionTableFull)?;
        let (id, _) = self.entries.insert_full(entry);
        let id = u32::try_from(id).map_err(|_| DynamicProfileError::FunctionTableFull)?;
        Ok(DynamicFunctionIndex { value: id })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct StoredStackTrace {
    packed_functions: &'static [u64],
    lines: &'static [u32],
}

impl StoredStackTrace {
    fn new(
        locations: &[DynamicLocation],
        arena: &ChainAllocator<VirtualAllocator>,
    ) -> Result<Self, DynamicProfileError> {
        let word_count = locations.len().div_ceil(PACKED_FUNCTIONS_PER_WORD);
        let packed_functions = try_allocate_arena_slice::<u64>(arena, word_count)
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        let lines = try_allocate_arena_slice::<u32>(arena, locations.len())
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        for (offset, location) in locations.iter().enumerate() {
            let word = offset / PACKED_FUNCTIONS_PER_WORD;
            let shift = (offset % PACKED_FUNCTIONS_PER_WORD) * PACKED_FUNCTION_BITS;
            packed_functions[word] |= u64::from(location.function.value) << shift;
            lines[offset] = location.line;
        }
        Ok(Self {
            packed_functions: unsafe {
                core::mem::transmute::<&[u64], &'static [u64]>(packed_functions)
            },
            lines: unsafe { core::mem::transmute::<&[u32], &'static [u32]>(lines) },
        })
    }

    fn location_ids(&self) -> Vec<u64> {
        self.lines
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                let word = self.packed_functions[offset / PACKED_FUNCTIONS_PER_WORD];
                let shift = (offset % PACKED_FUNCTIONS_PER_WORD) * PACKED_FUNCTION_BITS;
                let function = ((word >> shift) & PACKED_FUNCTION_MASK) as u32;
                (u64::from(function) << 32) | u64::from(*line)
            })
            .collect()
    }
}

fn try_allocate_arena_slice<'a, T: Copy>(
    arena: &'a ChainAllocator<VirtualAllocator>,
    len: usize,
) -> Result<&'a mut [T], ()> {
    if len == 0 {
        return Ok(&mut []);
    }

    let layout = Layout::array::<T>(len).map_err(|_| ())?;
    let allocation = arena.allocate(layout).map_err(|_| ())?;
    let ptr = allocation.as_ptr() as *mut T;

    unsafe {
        ptr.write_bytes(0, len);
        Ok(slice::from_raw_parts_mut(ptr, len))
    }
}

fn encode_timestamped_record_to_writer(
    writer: &mut impl Write,
    label_set_id: u32,
    timestamp_delta_ns: i32,
    values: &[i64],
) -> io::Result<()> {
    writer.write_all(&label_set_id.to_ne_bytes())?;
    writer.write_all(&timestamp_delta_ns.to_ne_bytes())?;
    for value in values {
        writer.write_all(&value.to_ne_bytes())?;
    }
    Ok(())
}

struct DynamicCompressedTimestampedSamples<C: ObservationCodec = DefaultCodec> {
    compressed_data: BufWriter<C::Encoder>,
    sample_types_len: usize,
    count: usize,
}

struct DynamicCompressedTimestampedSamplesIter<C: ObservationCodec> {
    decoder: C::Decoder,
    sample_types_len: usize,
}

impl<C: ObservationCodec> DynamicCompressedTimestampedSamples<C> {
    const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024;
    const MAX_CAPACITY: usize = i32::MAX as usize;

    fn try_new(sample_types_len: usize) -> io::Result<Self> {
        Ok(Self {
            compressed_data: BufWriter::with_capacity(
                C::recommended_input_buf_size(),
                C::new_encoder(Self::DEFAULT_BUFFER_SIZE, Self::MAX_CAPACITY)?,
            ),
            sample_types_len,
            count: 0,
        })
    }

    fn add(
        &mut self,
        stacktrace: u32,
        label_set_id: u32,
        timestamp_delta_ns: i32,
        values: &[i64],
    ) -> io::Result<()> {
        self.compressed_data.write_all(&stacktrace.to_ne_bytes())?;
        encode_timestamped_record_to_writer(
            &mut self.compressed_data,
            label_set_id,
            timestamp_delta_ns,
            values,
        )?;
        self.count += 1;
        Ok(())
    }

    fn count(&self) -> usize {
        self.count
    }

    fn try_into_iter(self) -> io::Result<DynamicCompressedTimestampedSamplesIter<C>> {
        let encoder = self
            .compressed_data
            .into_inner()
            .map_err(|e| e.into_error())?;
        Ok(DynamicCompressedTimestampedSamplesIter {
            decoder: C::encoder_into_decoder(encoder)?,
            sample_types_len: self.sample_types_len,
        })
    }
}

impl<C: ObservationCodec> Iterator for DynamicCompressedTimestampedSamplesIter<C> {
    type Item = io::Result<(u32, u32, i64, Vec<i64>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut stacktrace = [0u8; 4];
        match self.decoder.read_exact(&mut stacktrace) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(error) => return Some(Err(error)),
        }
        let stacktrace = u32::from_ne_bytes(stacktrace);

        let mut label_set_id = [0u8; 4];
        if let Err(error) = self.decoder.read_exact(&mut label_set_id) {
            return Some(Err(error));
        }
        let label_set_id = u32::from_ne_bytes(label_set_id);

        let mut timestamp_delta_ns = [0u8; 4];
        if let Err(error) = self.decoder.read_exact(&mut timestamp_delta_ns) {
            return Some(Err(error));
        }
        let timestamp_delta_ns = i64::from(i32::from_ne_bytes(timestamp_delta_ns));

        let mut values = Vec::with_capacity(self.sample_types_len);
        for _ in 0..self.sample_types_len {
            let mut value = [0u8; 8];
            if let Err(error) = self.decoder.read_exact(&mut value) {
                return Some(Err(error));
            }
            values.push(i64::from_ne_bytes(value));
        }
        Some(Ok((stacktrace, label_set_id, timestamp_delta_ns, values)))
    }
}

fn allocate_owned_locations(
    arena: &ChainAllocator<VirtualAllocator>,
    locations: &[DynamicLocation],
) -> Result<&'static [DynamicLocation], DynamicProfileError> {
    let owned = try_allocate_arena_slice::<DynamicLocation>(arena, locations.len())
        .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
    owned.copy_from_slice(locations);
    Ok(unsafe { core::mem::transmute::<&[DynamicLocation], &'static [DynamicLocation]>(owned) })
}

#[derive(Copy, Clone, Debug)]
struct DynamicLocationSlice<'a>(&'a [DynamicLocation]);

impl DynamicLocationSlice<'_> {
    fn as_bytes(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), core::mem::size_of_val(self.0))
        }
    }
}

impl PartialEq for DynamicLocationSlice<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for DynamicLocationSlice<'_> {}

impl std::hash::Hash for DynamicLocationSlice<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write(self.as_bytes());
    }
}

struct DynamicStackTraceTable {
    arena: ChainAllocator<VirtualAllocator>,
    cache: FxIndexMap<DynamicLocationSlice<'static>, u32>,
    entries: FxIndexSet<StoredStackTrace>,
}

impl DynamicStackTraceTable {
    fn new() -> Self {
        let arena = ChainAllocator::new_in(1024 * 1024, VirtualAllocator {});
        let mut cache = FxIndexMap::default();
        cache.reserve(28);
        cache.insert(DynamicLocationSlice(&[]), 0);
        let mut entries = FxIndexSet::default();
        entries.reserve(28);
        entries.insert(StoredStackTrace {
            packed_functions: &[],
            lines: &[],
        });
        Self {
            arena,
            cache,
            entries,
        }
    }
    fn get(&self, index: DynamicStackTraceIndex) -> Option<&StoredStackTrace> {
        self.entries.get_index(index.value as usize)
    }

    fn iter_non_empty(&self) -> impl Iterator<Item = &StoredStackTrace> {
        self.entries.iter().skip(1)
    }

    fn intern(
        &mut self,
        locations: &[DynamicLocation],
    ) -> Result<DynamicStackTraceIndex, DynamicProfileError> {
        self.cache
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        let locations = DynamicLocationSlice(locations);
        let hash = self.cache.hasher().hash_one(locations);
        match self
            .cache
            .raw_entry_mut_v1()
            .from_hash(hash, |stored| *stored == locations)
        {
            RawEntryMut::Occupied(entry) => Ok(DynamicStackTraceIndex {
                value: *entry.get(),
            }),
            RawEntryMut::Vacant(entry) => {
                if self.entries.len() >= u32::MAX as usize {
                    return Err(DynamicProfileError::StackTraceTableFull);
                }
                let stacktrace = StoredStackTrace::new(locations.0, &self.arena)?;
                let owned_locations = allocate_owned_locations(&self.arena, locations.0)?;
                self.entries
                    .try_reserve(1)
                    .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
                let (id, _) = self.entries.insert_full(stacktrace);
                let id = u32::try_from(id).map_err(|_| DynamicProfileError::StackTraceTableFull)?;
                entry.insert_hashed_nocheck(hash, DynamicLocationSlice(owned_locations), id);
                Ok(DynamicStackTraceIndex { value: id })
            }
        }
    }
}

struct DynamicLabelSetTable {
    arena: ChainAllocator<VirtualAllocator>,
    cache: FxIndexMap<StoredLabelSlice<'static>, u32>,
    entries: Vec<&'static [StoredLabel]>,
}

impl DynamicLabelSetTable {
    fn new() -> Self {
        let arena = ChainAllocator::new_in(256 * 1024, VirtualAllocator {});
        let mut cache = FxIndexMap::default();
        cache.reserve(28);
        cache.insert(StoredLabelSlice(&[]), 0);
        let mut entries: Vec<&'static [StoredLabel]> = Vec::new();
        entries.reserve(28);
        entries.push(&[] as &'static [StoredLabel]);
        Self {
            arena,
            cache,
            entries,
        }
    }

    fn get(&self, index: u32) -> Option<&[StoredLabel]> {
        self.entries.get(index as usize).copied()
    }

    fn intern(&mut self, labels: &[StoredLabel]) -> Result<u32, DynamicProfileError> {
        self.cache
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::LabelSetTableFull)?;
        let labels = StoredLabelSlice(labels);
        let hash = self.cache.hasher().hash_one(labels);
        match self
            .cache
            .raw_entry_mut_v1()
            .from_hash(hash, |stored| *stored == labels)
        {
            RawEntryMut::Occupied(entry) => Ok(*entry.get()),
            RawEntryMut::Vacant(entry) => {
                if self.entries.len() >= u32::MAX as usize {
                    return Err(DynamicProfileError::LabelSetTableFull);
                }
                let owned = try_allocate_arena_slice::<StoredLabel>(&self.arena, labels.0.len())
                    .map_err(|_| DynamicProfileError::LabelSetTableFull)?;
                owned.copy_from_slice(labels.0);
                let owned = unsafe {
                    core::mem::transmute::<&[StoredLabel], &'static [StoredLabel]>(owned)
                };
                self.entries
                    .try_reserve(1)
                    .map_err(|_| DynamicProfileError::LabelSetTableFull)?;
                let id = u32::try_from(self.entries.len())
                    .map_err(|_| DynamicProfileError::LabelSetTableFull)?;
                self.entries.push(owned);
                entry.insert_hashed_nocheck(hash, StoredLabelSlice(owned), id);
                Ok(id)
            }
        }
    }
}

#[derive(Default)]
struct DynamicUpscalingRules {
    rules: FxHashMap<(u32, u32), Vec<DynamicUpscalingRule>>,
    by_label_offsets: FxHashSet<usize>,
}

#[derive(Debug)]
struct DynamicUpscalingRule {
    values_offset: Vec<usize>,
    upscaling_info: api::UpscalingInfo,
}

impl DynamicUpscalingRule {
    fn compute_scale(&self, values: &[i64]) -> f64 {
        match self.upscaling_info {
            api::UpscalingInfo::Poisson {
                sum_value_offset,
                count_value_offset,
                sampling_distance,
            } => {
                if values[sum_value_offset] == 0 || values[count_value_offset] == 0 {
                    return 1.0;
                }
                let avg = values[sum_value_offset] as f64 / values[count_value_offset] as f64;
                1_f64 / (1_f64 - (-avg / sampling_distance as f64).exp())
            }
            api::UpscalingInfo::PoissonNonSampleTypeCount {
                sum_value_offset,
                count_value,
                sampling_distance,
            } => {
                if values[sum_value_offset] == 0 || count_value == 0 {
                    return 1.0;
                }
                let avg = values[sum_value_offset] as f64 / count_value as f64;
                1_f64 / (1_f64 - (-avg / sampling_distance as f64).exp())
            }
            api::UpscalingInfo::Proportional { scale } => scale,
        }
    }
}

impl DynamicUpscalingRules {
    fn add(
        &mut self,
        values_offset: &[usize],
        label_key: DynamicStringIndex,
        label_value: u32,
        upscaling_info: api::UpscalingInfo,
        max_offset: usize,
    ) -> Result<(), DynamicProfileError> {
        let mut normalized = Vec::new();
        normalized
            .try_reserve_exact(values_offset.len())
            .map_err(|_| DynamicProfileError::InvalidUpscalingOffset {
                offset: 0,
                max: max_offset,
            })?;
        normalized.extend_from_slice(values_offset);
        normalized.sort_unstable();
        for offset in &normalized {
            if *offset >= max_offset {
                return Err(DynamicProfileError::InvalidUpscalingOffset {
                    offset: *offset,
                    max: max_offset,
                });
            }
        }

        let rule_key = (label_key.value, label_value);
        let is_by_label = rule_key != (0, 0);
        if is_by_label {
            for offset in &normalized {
                if self
                    .rules
                    .get(&(0, 0))
                    .is_some_and(|rules| rules.iter().any(|r| r.values_offset.contains(offset)))
                {
                    return Err(DynamicProfileError::InvalidUpscalingOffset {
                        offset: *offset,
                        max: max_offset,
                    });
                }
            }
        } else {
            for offset in &normalized {
                if self.by_label_offsets.contains(offset) {
                    return Err(DynamicProfileError::InvalidUpscalingOffset {
                        offset: *offset,
                        max: max_offset,
                    });
                }
            }
        }

        if is_by_label {
            for offset in &normalized {
                self.by_label_offsets.insert(*offset);
            }
        }

        self.rules
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::InvalidUpscalingOffset {
                offset: 0,
                max: max_offset,
            })?;
        let rule = DynamicUpscalingRule {
            values_offset: normalized,
            upscaling_info,
        };
        if let Some(existing) = self.rules.get_mut(&rule_key) {
            existing
                .try_reserve(1)
                .map_err(|_| DynamicProfileError::InvalidUpscalingOffset {
                    offset: 0,
                    max: max_offset,
                })?;
            existing.push(rule);
        } else {
            let mut rules = Vec::new();
            rules
                .try_reserve(1)
                .map_err(|_| DynamicProfileError::InvalidUpscalingOffset {
                    offset: 0,
                    max: max_offset,
                })?;
            rules.push(rule);
            self.rules.insert(rule_key, rules);
        }
        Ok(())
    }

    fn upscale_values(&self, values: &mut [i64], labels: &[StoredLabel]) {
        let mut matching_rules = Vec::new();
        for label in labels {
            let value = label.str;
            if let Some(rules) = self.rules.get(&(label.key, value)) {
                matching_rules.push(rules);
            }
        }
        if let Some(rules) = self.rules.get(&(0, 0)) {
            matching_rules.push(rules);
        }

        for rules in matching_rules {
            for rule in rules {
                let scale = rule.compute_scale(values);
                for offset in &rule.values_offset {
                    values[*offset] = (values[*offset] as f64 * scale).round() as i64;
                }
            }
        }
    }
}

struct PeriodLocalData {
    private_strings: DynamicStringTable,
    label_sets: DynamicLabelSetTable,
    stacktraces: DynamicStackTraceTable,
    aggregated: FxHashMap<SampleKey, Vec<i64>>,
    timestamped: DynamicCompressedTimestampedSamples,
    endpoints: FxHashMap<u64, u32>,
    endpoint_stats: ProfiledEndpointsStats,
    upscaling_rules: DynamicUpscalingRules,
}

impl PeriodLocalData {
    fn try_new(sample_types_len: usize) -> Result<Self, DynamicProfileError> {
        Ok(Self {
            private_strings: DynamicStringTable::new(),
            label_sets: DynamicLabelSetTable::new(),
            stacktraces: DynamicStackTraceTable::new(),
            aggregated: FxHashMap::default(),
            timestamped: DynamicCompressedTimestampedSamples::try_new(sample_types_len)
                .map_err(DynamicProfileError::TimestampedObservationIo)?,
            endpoints: FxHashMap::default(),
            endpoint_stats: ProfiledEndpointsStats::default(),
            upscaling_rules: DynamicUpscalingRules::default(),
        })
    }
}

#[derive(Copy, Clone)]
struct WellKnownPublicStrings {
    local_root_span_id: DynamicStringIndex,
    trace_endpoint: DynamicStringIndex,
    end_timestamp_ns: DynamicStringIndex,
}

pub struct DynamicProfile {
    sample_types: Box<[api::SampleType]>,
    period: Option<api::Period>,
    start_time: SystemTime,
    public_strings: DynamicStringTable,
    functions: DynamicFunctionTable,
    period_local: PeriodLocalData,
    well_known: WellKnownPublicStrings,
}

impl DynamicProfile {
    pub fn try_new(
        sample_types: &[api::SampleType],
        period: Option<api::Period>,
        start_time: Option<SystemTime>,
    ) -> Result<Self, DynamicProfileError> {
        let mut profile = Self {
            sample_types: sample_types.to_vec().into_boxed_slice(),
            period,
            start_time: start_time.unwrap_or_else(SystemTime::now),
            public_strings: DynamicStringTable::new(),
            functions: DynamicFunctionTable::new(),
            period_local: PeriodLocalData::try_new(sample_types.len())?,
            well_known: WellKnownPublicStrings {
                local_root_span_id: DynamicStringIndex::EMPTY,
                trace_endpoint: DynamicStringIndex::EMPTY,
                end_timestamp_ns: DynamicStringIndex::EMPTY,
            },
        };
        profile.reinitialize_public_strings()?;
        Ok(profile)
    }

    fn reinitialize_public_strings(&mut self) -> Result<(), DynamicProfileError> {
        self.public_strings = DynamicStringTable::new();
        self.well_known = WellKnownPublicStrings {
            local_root_span_id: DynamicStringIndex {
                value: self.public_strings.intern("local root span id")?,
            },
            trace_endpoint: DynamicStringIndex {
                value: self.public_strings.intern("trace endpoint")?,
            },
            end_timestamp_ns: DynamicStringIndex {
                value: self.public_strings.intern("end_timestamp_ns")?,
            },
        };
        for sample_type in self.sample_types.iter().copied() {
            let value_type: api::ValueType<'static> = sample_type.into();
            self.public_strings.intern(value_type.r#type)?;
            self.public_strings.intern(value_type.unit)?;
        }
        if let Some(period) = self.period {
            let value_type: api::ValueType<'static> = period.sample_type.into();
            self.public_strings.intern(value_type.r#type)?;
            self.public_strings.intern(value_type.unit)?;
        }
        Ok(())
    }

    pub fn intern_string(&mut self, s: &str) -> Result<DynamicStringIndex, DynamicProfileError> {
        Ok(DynamicStringIndex {
            value: self.public_strings.intern(s)?,
        })
    }

    pub fn intern_function(
        &mut self,
        name: DynamicStringIndex,
        filename: DynamicStringIndex,
    ) -> Result<DynamicFunctionIndex, DynamicProfileError> {
        self.ensure_public_string(name)?;
        self.ensure_public_string(filename)?;
        self.functions.intern(name, filename)
    }

    pub fn intern_stacktrace(
        &mut self,
        locations: &[DynamicLocation],
    ) -> Result<DynamicStackTraceIndex, DynamicProfileError> {
        self.validate_locations(locations)?;
        self.period_local.stacktraces.intern(locations)
    }

    pub fn add_sample_by_stacktrace(
        &mut self,
        stacktrace: DynamicStackTraceIndex,
        sample: DynamicSample<'_>,
        timestamp_ns: i64,
    ) -> Result<(), DynamicProfileError> {
        self.ensure_stacktrace(stacktrace)?;
        let key = SampleKey {
            stacktrace: stacktrace.value,
            labels: self.store_labels(sample.labels)?,
        };
        self.add_sample(key, sample.values, timestamp_ns)
    }

    pub fn add_sample_by_locations(
        &mut self,
        locations: &[DynamicLocation],
        sample: DynamicSample<'_>,
        timestamp_ns: i64,
    ) -> Result<(), DynamicProfileError> {
        let stacktrace = self.intern_stacktrace(locations)?;
        self.add_sample_by_stacktrace(stacktrace, sample, timestamp_ns)
    }

    pub fn set_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: &str,
    ) -> Result<(), DynamicProfileError> {
        let endpoint_id = self.period_local.private_strings.intern(endpoint)?;
        self.period_local
            .endpoints
            .insert(local_root_span_id, endpoint_id);
        Ok(())
    }

    pub fn add_endpoint_count(
        &mut self,
        endpoint: &str,
        value: i64,
    ) -> Result<(), DynamicProfileError> {
        self.period_local
            .endpoint_stats
            .add_endpoint_count(endpoint.to_owned(), value);
        Ok(())
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_key: DynamicStringIndex,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> Result<(), DynamicProfileError> {
        self.ensure_public_string(label_key)?;
        let value_id = self.period_local.private_strings.intern(label_value)?;
        self.period_local.upscaling_rules.add(
            offset_values,
            label_key,
            value_id,
            api::UpscalingInfo::Poisson {
                sum_value_offset,
                count_value_offset,
                sampling_distance,
            },
            self.sample_types.len(),
        )
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_key: DynamicStringIndex,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> Result<(), DynamicProfileError> {
        self.ensure_public_string(label_key)?;
        let value_id = self.period_local.private_strings.intern(label_value)?;
        self.period_local.upscaling_rules.add(
            offset_values,
            label_key,
            value_id,
            api::UpscalingInfo::PoissonNonSampleTypeCount {
                sum_value_offset,
                count_value,
                sampling_distance,
            },
            self.sample_types.len(),
        )
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_key: DynamicStringIndex,
        label_value: &str,
        scale: f64,
    ) -> Result<(), DynamicProfileError> {
        self.ensure_public_string(label_key)?;
        let value_id = self.period_local.private_strings.intern(label_value)?;
        self.period_local.upscaling_rules.add(
            offset_values,
            label_key,
            value_id,
            api::UpscalingInfo::Proportional { scale },
            self.sample_types.len(),
        )
    }

    pub fn serialize_and_clear_period_local_data(
        &mut self,
        end_time: Option<SystemTime>,
        duration: Option<Duration>,
    ) -> Result<EncodedProfile, DynamicProfileError> {
        let _end = end_time.unwrap_or_else(SystemTime::now);
        let start = self.start_time;
        let duration_nanos = duration
            .unwrap_or_else(|| _end.duration_since(start).unwrap_or(Duration::ZERO))
            .as_nanos()
            .min(i64::MAX as u128) as i64;
        let profile = self.materialize_pprof(start, _end, duration_nanos)?;
        let mut buffer = Vec::with_capacity(profile.encoded_len());
        profile.encode(&mut buffer)?;
        let compressed = zstd::stream::encode_all(
            std::io::Cursor::new(buffer),
            NativeProfile::COMPRESSION_LEVEL,
        )
        .map_err(DynamicProfileError::Compression)?;
        let encoded = EncodedProfile {
            start,
            end: _end,
            buffer: compressed,
            endpoints_stats: self.period_local.endpoint_stats.clone(),
        };
        self.clear_period_local_data()?;
        Ok(encoded)
    }

    pub fn clear_period_local_data(&mut self) -> Result<(), DynamicProfileError> {
        self.period_local = PeriodLocalData::try_new(self.sample_types.len())?;
        self.start_time = SystemTime::now();
        Ok(())
    }

    pub fn clear_all_data(&mut self) -> Result<(), DynamicProfileError> {
        self.functions.clear_all();
        self.period_local = PeriodLocalData::try_new(self.sample_types.len())?;
        self.reinitialize_public_strings()?;
        self.start_time = SystemTime::now();
        Ok(())
    }

    fn ensure_public_string(&self, index: DynamicStringIndex) -> Result<(), DynamicProfileError> {
        if self.public_strings.get(index.value).is_some() {
            Ok(())
        } else {
            Err(DynamicProfileError::InvalidStringIndex { index: index.value })
        }
    }

    fn ensure_stacktrace(&self, index: DynamicStackTraceIndex) -> Result<(), DynamicProfileError> {
        if self.period_local.stacktraces.get(index).is_some() {
            Ok(())
        } else {
            Err(DynamicProfileError::InvalidStackTraceIndex { index: index.value })
        }
    }

    fn validate_locations(&self, locations: &[DynamicLocation]) -> Result<(), DynamicProfileError> {
        for location in locations {
            if location.function.is_empty() {
                return Err(DynamicProfileError::EmptyFunctionInLocation);
            }
            if self.functions.get(location.function).is_none() {
                return Err(DynamicProfileError::InvalidFunctionIndex {
                    index: location.function.value,
                });
            }
        }
        Ok(())
    }

    fn store_labels(&mut self, labels: &[DynamicLabel<'_>]) -> Result<u32, DynamicProfileError> {
        debug_assert!(
            labels_have_unique_keys(labels),
            "label keys must be unique within a sample"
        );
        let mut stored = SmallVec::<[StoredLabel; 4]>::new();
        stored
            .try_reserve_exact(labels.len())
            .map_err(|_| DynamicProfileError::LabelSetTableFull)?;
        for label in labels {
            self.ensure_public_string(label.key)?;
            if !label.uses_at_most_one_of_str_and_num() {
                return Err(DynamicProfileError::InvalidLabelValue);
            }
            let (str, num) = if label.str.is_empty() {
                (0, label.num)
            } else {
                (self.period_local.private_strings.intern(label.str)?, 0)
            };
            stored.push(StoredLabel {
                key: label.key.value,
                str,
                num,
            });
        }
        self.period_local.label_sets.intern(&stored)
    }

    fn add_sample(
        &mut self,
        key: SampleKey,
        values: &[i64],
        timestamp_ns: i64,
    ) -> Result<(), DynamicProfileError> {
        if values.len() != self.sample_types.len() {
            return Err(DynamicProfileError::ValuesLengthMismatch {
                expected: self.sample_types.len(),
                actual: values.len(),
            });
        }
        if timestamp_ns == 0 {
            let entry = self
                .period_local
                .aggregated
                .entry(key)
                .or_insert_with(|| vec![0; values.len()]);
            for (dst, src) in entry.iter_mut().zip(values.iter()) {
                *dst = dst.saturating_add(*src);
            }
        } else {
            let timestamp_delta_ns = i32::try_from(timestamp_ns).map_err(|_| {
                DynamicProfileError::TimestampDeltaOutOfRange {
                    value: timestamp_ns,
                }
            })?;
            self.period_local
                .timestamped
                .add(key.stacktrace, key.labels, timestamp_delta_ns, values)
                .map_err(DynamicProfileError::TimestampedObservationIo)?;
        }
        Ok(())
    }

    fn public_value_type(
        &self,
        sample_type: api::SampleType,
    ) -> Result<pprof::ValueType, DynamicProfileError> {
        let value_type: api::ValueType<'static> = sample_type.into();
        let ty = self
            .public_strings
            .id_for(value_type.r#type)
            .ok_or(DynamicProfileError::StringTableFull)?;
        let unit = self
            .public_strings
            .id_for(value_type.unit)
            .ok_or(DynamicProfileError::StringTableFull)?;
        Ok(pprof::ValueType {
            r#type: i64::from(ty),
            unit: i64::from(unit),
        })
    }

    fn private_string_to_pprof_index(&self, private_id: u32) -> i64 {
        if private_id == 0 {
            0
        } else {
            (self.public_strings.len() + (private_id as usize) - 1) as i64
        }
    }

    fn materialize_pprof(
        &mut self,
        start: SystemTime,
        _end: SystemTime,
        duration_nanos: i64,
    ) -> Result<pprof::Profile, DynamicProfileError> {
        let total_strings =
            self.public_strings.len() + self.period_local.private_strings.len().saturating_sub(1);
        let mut string_table = Vec::new();
        string_table
            .try_reserve(total_strings)
            .map_err(|_| DynamicProfileError::StringTableFull)?;
        string_table.extend(self.public_strings.iter().map(str::to_owned));
        string_table.extend(
            self.period_local
                .private_strings
                .iter()
                .skip(1)
                .map(str::to_owned),
        );

        let sample_types = self
            .sample_types
            .iter()
            .copied()
            .map(|sample_type| self.public_value_type(sample_type))
            .collect::<Result<Vec<_>, _>>()?;
        let period_type = self
            .period
            .map(|period| self.public_value_type(period.sample_type))
            .transpose()?;
        let period = self.period.map_or(0, |period| period.value);

        let functions = self
            .functions
            .iter_non_empty()
            .map(|(id, function)| pprof::Function {
                id: u64::from(id),
                name: i64::from(function.name),
                system_name: 0,
                filename: i64::from(function.filename),
            })
            .collect();

        let mut unique_locations = FxHashSet::default();
        unique_locations
            .try_reserve(
                self.period_local
                    .stacktraces
                    .entries
                    .len()
                    .saturating_sub(1),
            )
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        for stacktrace in self.period_local.stacktraces.iter_non_empty() {
            unique_locations.extend(stacktrace.location_ids());
        }
        let locations = unique_locations
            .into_iter()
            .map(|location_id| pprof::Location {
                id: location_id,
                mapping_id: 0,
                address: 0,
                lines: vec![pprof::Line {
                    function_id: location_id >> 32,
                    line: (location_id & 0xffff_ffff) as i64,
                }],
                is_folded: false,
            })
            .collect();

        let empty_timestamped =
            DynamicCompressedTimestampedSamples::try_new(self.sample_types.len())
                .map_err(DynamicProfileError::TimestampedObservationIo)?;
        let timestamped = std::mem::replace(&mut self.period_local.timestamped, empty_timestamped);
        let mut samples =
            Vec::with_capacity(self.period_local.aggregated.len() + timestamped.count());
        let timestamped_iter = timestamped
            .try_into_iter()
            .map_err(DynamicProfileError::TimestampedObservationIo)?;
        for item in timestamped_iter {
            let (stacktrace, labels, timestamp_delta_ns, values) =
                item.map_err(DynamicProfileError::TimestampedObservationIo)?;
            let key = SampleKey { stacktrace, labels };
            self.materialize_sample(&mut samples, &key, timestamp_delta_ns, &values)?;
        }
        for (key, values) in &self.period_local.aggregated {
            self.materialize_sample(&mut samples, key, 0, values)?;
        }

        let time_nanos = start
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_nanos().min(i64::MAX as u128) as i64
            });
        Ok(pprof::Profile {
            sample_types,
            samples,
            mappings: Vec::new(),
            locations,
            functions,
            string_table,
            drop_frames: 0,
            keep_frames: 0,
            time_nanos,
            duration_nanos,
            period_type,
            period,
            comment: Vec::new(),
            default_sample_type: 0,
        })
    }

    fn materialize_sample(
        &self,
        samples: &mut Vec<pprof::Sample>,
        key: &SampleKey,
        timestamp: i64,
        values: &[i64],
    ) -> Result<(), DynamicProfileError> {
        let stacktrace = self
            .period_local
            .stacktraces
            .get(DynamicStackTraceIndex {
                value: key.stacktrace,
            })
            .ok_or(DynamicProfileError::InvalidStackTraceIndex {
                index: key.stacktrace,
            })?;
        let mut labels = self
            .period_local
            .label_sets
            .get(key.labels)
            .ok_or(DynamicProfileError::InvalidLabelSetIndex { index: key.labels })?
            .to_vec();
        if let Some(endpoint_id) = self.endpoint_for_labels(&labels) {
            labels.push(StoredLabel {
                key: self.well_known.trace_endpoint.value,
                str: endpoint_id,
                num: 0,
            });
        }
        let mut sample_values = values.to_vec();
        self.period_local
            .upscaling_rules
            .upscale_values(&mut sample_values, &labels);
        if timestamp != 0 {
            labels.push(StoredLabel {
                key: self.well_known.end_timestamp_ns.value,
                str: 0,
                num: timestamp,
            });
        }
        let labels = labels
            .into_iter()
            .map(|label| self.to_pprof_label(label))
            .collect();
        samples.push(pprof::Sample {
            location_ids: stacktrace.location_ids(),
            values: sample_values,
            labels,
        });
        Ok(())
    }

    fn to_pprof_label(&self, label: StoredLabel) -> pprof::Label {
        if label.str != 0 {
            pprof::Label {
                key: i64::from(label.key),
                str: self.private_string_to_pprof_index(label.str),
                num: 0,
                num_unit: 0,
            }
        } else {
            pprof::Label {
                key: i64::from(label.key),
                str: 0,
                num: label.num,
                num_unit: 0,
            }
        }
    }

    fn endpoint_for_labels(&self, labels: &[StoredLabel]) -> Option<u32> {
        labels.iter().find_map(|label| {
            if label.key != self.well_known.local_root_span_id.value {
                return None;
            }
            if label.str != 0 {
                return None;
            }
            let local_root_span_id = label.num as u64;
            self.period_local
                .endpoints
                .get(&local_root_span_id)
                .copied()
        })
    }
}

fn labels_have_unique_keys(labels: &[DynamicLabel<'_>]) -> bool {
    for (index, label) in labels.iter().enumerate() {
        if labels[..index]
            .iter()
            .any(|seen| seen.key.value == label.key.value)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use std::collections::HashSet;
    use std::mem::align_of;

    fn decode_profile(buffer: &[u8]) -> pprof::Profile {
        let decoded =
            zstd::stream::decode_all(std::io::Cursor::new(buffer)).expect("profile to decompress");
        pprof::Profile::decode(decoded.as_slice()).expect("profile to decode")
    }

    fn sample_types() -> [api::SampleType; 2] {
        [api::SampleType::WallTime, api::SampleType::WallSamples]
    }

    fn string_index(profile: &pprof::Profile, value: &str) -> i64 {
        profile
            .string_table
            .iter()
            .position(|item| item == value)
            .expect("string to exist") as i64
    }

    fn has_function(profile: &pprof::Profile, name: &str, filename: &str) -> bool {
        let name = string_index(profile, name);
        let filename = string_index(profile, filename);
        profile
            .functions
            .iter()
            .any(|function| function.name == name && function.filename == filename)
    }

    fn has_location(profile: &pprof::Profile, function_name: &str, line: i64) -> bool {
        let function_ids = profile
            .functions
            .iter()
            .filter(|function| {
                let name = profile.string_table.get(function.name as usize);
                name.is_some_and(|name| name == function_name)
            })
            .map(|function| function.id)
            .collect::<HashSet<_>>();
        profile.locations.iter().any(|location| {
            location.lines.iter().any(|line_info| {
                function_ids.contains(&line_info.function_id) && line_info.line == line
            })
        })
    }

    #[test]
    fn constructor_interns_well_known_strings() {
        let profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        assert_eq!(profile.public_strings.get(0), Some(""));
        assert_eq!(
            profile
                .public_strings
                .get(profile.well_known.local_root_span_id.value),
            Some("local root span id")
        );
        assert_eq!(
            profile
                .public_strings
                .get(profile.well_known.trace_endpoint.value),
            Some("trace endpoint")
        );
        assert_eq!(
            profile
                .public_strings
                .get(profile.well_known.end_timestamp_ns.value),
            Some("end_timestamp_ns")
        );
    }

    #[test]
    fn function_indices_follow_insertion_order_and_dedup() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.rb").expect("file");
        let first_name = profile.intern_string("first").expect("first");
        let second_name = profile.intern_string("second").expect("second");

        let first = profile
            .intern_function(first_name, file)
            .expect("first function");
        let second = profile
            .intern_function(second_name, file)
            .expect("second function");
        let first_again = profile
            .intern_function(first_name, file)
            .expect("first function again");

        assert_eq!(first.value, 1);
        assert_eq!(second.value, 2);
        assert_eq!(first, first_again);
    }

    #[test]
    fn stacktrace_indices_follow_insertion_order_and_dedup() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.rb").expect("file");
        let leaf_name = profile.intern_string("leaf").expect("leaf");
        let root_name = profile.intern_string("root").expect("root");
        let leaf = profile.intern_function(leaf_name, file).expect("leaf fn");
        let root = profile.intern_function(root_name, file).expect("root fn");

        let first = [DynamicLocation {
            function: leaf,
            line: 10,
        }];
        let second = [
            DynamicLocation {
                function: leaf,
                line: 10,
            },
            DynamicLocation {
                function: root,
                line: 20,
            },
        ];

        let first_id = profile.intern_stacktrace(&first).expect("first stack");
        let second_id = profile.intern_stacktrace(&second).expect("second stack");
        let first_again = profile
            .intern_stacktrace(&first)
            .expect("first stack again");

        assert_eq!(first_id.value, 1);
        assert_eq!(second_id.value, 2);
        assert_eq!(first_id, first_again);
    }

    #[test]
    fn stored_stacktraces_use_aligned_arena_slices() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.rb").expect("file");
        let leaf_name = profile.intern_string("leaf").expect("leaf");
        let mid_name = profile.intern_string("mid").expect("mid");
        let root_name = profile.intern_string("root").expect("root");
        let leaf = profile.intern_function(leaf_name, file).expect("leaf fn");
        let mid = profile.intern_function(mid_name, file).expect("mid fn");
        let root = profile.intern_function(root_name, file).expect("root fn");

        let stack = [
            DynamicLocation {
                function: leaf,
                line: 10,
            },
            DynamicLocation {
                function: mid,
                line: 20,
            },
            DynamicLocation {
                function: root,
                line: 30,
            },
            DynamicLocation {
                function: leaf,
                line: 40,
            },
        ];

        let stack_id = profile.intern_stacktrace(&stack).expect("stack");
        let stored = profile
            .period_local
            .stacktraces
            .get(stack_id)
            .expect("stored stacktrace");

        assert_eq!(
            stored.packed_functions.as_ptr() as usize % align_of::<u64>(),
            0
        );
        assert_eq!(stored.lines.as_ptr() as usize % align_of::<u32>(), 0);
        assert_eq!(stored.lines, &[10, 20, 30, 40]);
        assert_eq!(
            stored.location_ids(),
            vec![
                (u64::from(leaf.value) << 32) | 10,
                (u64::from(mid.value) << 32) | 20,
                (u64::from(root.value) << 32) | 30,
                (u64::from(leaf.value) << 32) | 40,
            ]
        );
    }

    #[test]
    fn public_indices_and_functions_persist_across_period_clear() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.rb").expect("file");
        let func = profile.intern_function(name, file).expect("function");
        profile.clear_period_local_data().expect("clear");
        assert_eq!(profile.intern_string("func").expect("same name"), name);
        assert_eq!(
            profile.intern_function(name, file).expect("same func"),
            func
        );
    }

    #[test]
    fn clear_all_data_resets_public_handles() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        profile.clear_all_data().expect("clear all");
        assert!(matches!(
            profile.intern_function(name, DynamicStringIndex::EMPTY),
            Err(DynamicProfileError::InvalidStringIndex { .. })
        ));
    }

    #[test]
    fn add_sample_by_locations_and_stacktrace_match() {
        let mut a = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let mut b = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");

        let name_a = a.intern_string("func").expect("name");
        let file_a = a.intern_string("file.py").expect("file");
        let func_a = a.intern_function(name_a, file_a).expect("function");
        let name_b = b.intern_string("func").expect("name");
        let file_b = b.intern_string("file.py").expect("file");
        let func_b = b.intern_function(name_b, file_b).expect("function");

        let locations_a = [DynamicLocation {
            function: func_a,
            line: 42,
        }];
        let locations_b = [DynamicLocation {
            function: func_b,
            line: 42,
        }];
        let stack_b = b.intern_stacktrace(&locations_b).expect("stack");

        let key_a = a.intern_string("thread id").expect("key");
        let key_b = b.intern_string("thread id").expect("key");
        let sample_a = DynamicSample {
            values: &[10, 1],
            labels: &[DynamicLabel {
                key: key_a,
                str: "",
                num: 7,
            }],
        };
        let sample_b = DynamicSample {
            values: &[10, 1],
            labels: &[DynamicLabel {
                key: key_b,
                str: "",
                num: 7,
            }],
        };

        a.add_sample_by_locations(&locations_a, sample_a, 0)
            .expect("add by locations");
        b.add_sample_by_stacktrace(stack_b, sample_b, 0)
            .expect("add by stacktrace");

        let profile_a = decode_profile(
            &a.serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        let profile_b = decode_profile(
            &b.serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );

        for exported in [&profile_a, &profile_b] {
            assert!(has_function(exported, "func", "file.py"));
            assert!(has_location(exported, "func", 42));
            assert!(exported
                .samples
                .iter()
                .any(|sample| sample.values == vec![10, 1]));
        }
    }

    #[test]
    fn timestamp_zero_aggregates_and_non_zero_is_preserved() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.rb").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 9,
        }];
        let sample = DynamicSample {
            values: &[3, 1],
            labels: &[],
        };

        profile
            .add_sample_by_locations(&location, sample, 0)
            .expect("first");
        profile
            .add_sample_by_locations(&location, sample, 0)
            .expect("second");
        profile
            .add_sample_by_locations(&location, sample, 12)
            .expect("timestamped");

        let encoded = profile
            .serialize_and_clear_period_local_data(None, None)
            .expect("serialize");
        let profile = decode_profile(&encoded.buffer);
        assert_eq!(profile.samples.len(), 2);
        assert!(profile
            .samples
            .iter()
            .any(|sample| sample.values == vec![6, 2]));
        assert!(profile
            .samples
            .iter()
            .any(|sample| sample.values == vec![3, 1]));
    }

    #[test]
    fn timestamped_samples_export_end_timestamp_label() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 33,
        }];

        profile
            .add_sample_by_locations(
                &location,
                DynamicSample {
                    values: &[5, 1],
                    labels: &[],
                },
                123_456,
            )
            .expect("timestamped sample");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        let timestamp_key = string_index(&profile, "end_timestamp_ns");

        assert!(profile.samples.iter().any(|sample| {
            sample
                .labels
                .iter()
                .any(|label| label.key == timestamp_key && label.num == 123_456)
        }));
    }

    #[test]
    fn negative_timestamped_samples_export_end_timestamp_label() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 34,
        }];

        profile
            .add_sample_by_locations(
                &location,
                DynamicSample {
                    values: &[5, 1],
                    labels: &[],
                },
                -123_456,
            )
            .expect("timestamped sample");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        let timestamp_key = string_index(&profile, "end_timestamp_ns");

        assert!(profile.samples.iter().any(|sample| {
            sample
                .labels
                .iter()
                .any(|label| label.key == timestamp_key && label.num == -123_456)
        }));
    }

    #[test]
    fn timestamp_out_of_i32_range_errors() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 35,
        }];

        let result = profile.add_sample_by_locations(
            &location,
            DynamicSample {
                values: &[5, 1],
                labels: &[],
            },
            i64::from(i32::MAX) + 1,
        );

        assert!(matches!(
            result,
            Err(DynamicProfileError::TimestampDeltaOutOfRange { .. })
        ));
    }

    #[test]
    fn numeric_labels_match_upscaling_rules_by_key_only() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.rb").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 17,
        }];
        let key = profile.intern_string("thread id").expect("key");

        profile
            .add_upscaling_rule_proportional(&[0], key, "", 2.0)
            .expect("upscaling rule");
        profile
            .add_sample_by_locations(
                &location,
                DynamicSample {
                    values: &[7, 1],
                    labels: &[DynamicLabel {
                        key,
                        str: "",
                        num: 9,
                    }],
                },
                0,
            )
            .expect("sample");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );

        assert!(profile
            .samples
            .iter()
            .any(|sample| sample.values == vec![14, 1]));
    }

    #[test]
    fn endpoint_enrichment_uses_private_strings() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.php").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let location = [DynamicLocation {
            function: func,
            line: 13,
        }];
        profile.set_endpoint(11, "/users/:id").expect("endpoint");
        let sample = DynamicSample {
            values: &[1, 1],
            labels: &[DynamicLabel {
                key: profile.well_known.local_root_span_id,
                str: "",
                num: 11,
            }],
        };
        profile
            .add_sample_by_locations(&location, sample, 0)
            .expect("sample");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        let endpoint_key = string_index(&profile, "trace endpoint");
        let endpoint_value = string_index(&profile, "/users/:id");
        assert!(profile.samples.iter().any(|sample| {
            sample
                .labels
                .iter()
                .any(|label| label.key == endpoint_key && label.str == endpoint_value)
        }));
    }
}
