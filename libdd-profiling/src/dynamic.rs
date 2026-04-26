// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;
use crate::collections::identifiable::{FxIndexMap, FxIndexSet};
use crate::collections::string_table::{self, StringTable};
use crate::internal::{EncodedProfile, Profile as NativeProfile, ProfiledEndpointsStats};
use crate::profiles::collections::Arc as ProfilesArc;
use crate::profiles::{
    Compressor, DefaultObservationCodec as DefaultCodec, DefaultProfileCodec, ObservationCodec,
};
use allocator_api2::alloc::AllocError;
use hashbrown::HashTable;
use indexmap::map::{raw_entry_v1::RawEntryMut, RawEntryApiV1};
use libdd_alloc::{Allocator, ChainAllocator, VirtualAllocator};
use libdd_profiling_protobuf::{
    self as protobuf, Record, StringOffset, Value, NO_OPT_ZERO, OPT_ZERO,
};
use parking_lot::lock_api::RawMutex as _;
use smallvec::SmallVec;
use std::alloc::Layout;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault};
use std::io::{self, BufWriter, Read, Write};
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::slice;
use std::str;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use thiserror::Error;

type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<rustc_hash::FxHasher>>;
type FxHashSet<V> = HashSet<V, BuildHasherDefault<rustc_hash::FxHasher>>;

const MAX_DYNAMIC_STRING_LENGTH: usize = (1 << 14) - 2;
const MAX_DYNAMIC_STRING_INDEX: usize = 1_835_008;
const DYNAMIC_STRING_INDEX_CAPACITY: usize = MAX_DYNAMIC_STRING_INDEX + 1;
const MAX_DYNAMIC_FUNCTION_INDEX: usize = 1_835_008;
const DYNAMIC_FUNCTION_INDEX_CAPACITY: usize = MAX_DYNAMIC_FUNCTION_INDEX + 1;
const MAX_DYNAMIC_LOCATION_INDEX: usize = (1 << 21) - 1;
const DYNAMIC_DICTIONARY_SEGMENT_BYTES: usize = 1 << 29;
const DYNAMIC_CONTROL_REGION_BYTES: usize = 512;
#[cfg(all(
    target_feature = "sse2",
    any(target_arch = "x86", target_arch = "x86_64"),
    not(miri),
))]
const DYNAMIC_HASH_GROUP_WIDTH: usize = 16;
#[cfg(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little",
    not(miri),
))]
const DYNAMIC_HASH_GROUP_WIDTH: usize = 8;
#[cfg(not(any(
    all(
        target_feature = "sse2",
        any(target_arch = "x86", target_arch = "x86_64"),
        not(miri),
    ),
    all(
        target_arch = "aarch64",
        target_feature = "neon",
        target_endian = "little",
        not(miri),
    ),
)))]
const DYNAMIC_HASH_GROUP_WIDTH: usize = core::mem::size_of::<usize>();
const DYNAMIC_HASH_TABLE_REGION_BYTES: usize = (18 * 1024 * 1024) + DYNAMIC_HASH_GROUP_WIDTH;
const DYNAMIC_STRING_ARRAY_BYTES: usize =
    DYNAMIC_STRING_INDEX_CAPACITY * core::mem::size_of::<AtomicU32>();
const DYNAMIC_FUNCTION_ARRAY_BYTES: usize =
    DYNAMIC_FUNCTION_INDEX_CAPACITY * core::mem::size_of::<AtomicU64>();
const DYNAMIC_STRING_RECORD_HEADER_BYTES: usize =
    core::mem::size_of::<u64>() + core::mem::size_of::<u16>();
const PACKED_STRING_ENTRY_OFFSET_BITS: u64 = 29;
const PACKED_STRING_ENTRY_LEN_BITS: u64 = 14;
const PACKED_STRING_ENTRY_INDEX_BITS: u64 = 21;
const PACKED_STRING_ENTRY_OFFSET_MASK: u64 = (1_u64 << PACKED_STRING_ENTRY_OFFSET_BITS) - 1;
const PACKED_STRING_ENTRY_LEN_MASK: u64 = (1_u64 << PACKED_STRING_ENTRY_LEN_BITS) - 1;
const PACKED_STRING_ENTRY_INDEX_MASK: u64 = (1_u64 << PACKED_STRING_ENTRY_INDEX_BITS) - 1;
const PACKED_FUNCTION_NAME_BITS: u64 = 21;
const PACKED_FUNCTION_FILENAME_BITS: u64 = 21;
const PACKED_FUNCTION_INDEX_BITS: u64 = 21;
const PACKED_FUNCTION_NAME_MASK: u64 = (1_u64 << PACKED_FUNCTION_NAME_BITS) - 1;
const PACKED_FUNCTION_FILENAME_MASK: u64 = (1_u64 << PACKED_FUNCTION_FILENAME_BITS) - 1;
const PACKED_FUNCTION_INDEX_MASK: u64 = (1_u64 << PACKED_FUNCTION_INDEX_BITS) - 1;
const PACKED_LOCATION_ID_BITS: usize = 21;
const PACKED_LOCATION_IDS_PER_WORD: usize = 3;
const PACKED_LOCATION_ID_MASK: u64 = (1_u64 << PACKED_LOCATION_ID_BITS) - 1;
const _: () = {
    assert!(DYNAMIC_CONTROL_REGION_BYTES % DYNAMIC_HASH_GROUP_WIDTH == 0);
};
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
    #[error("temporary serialization buffer allocation failed")]
    SerializationScratchAllocation,
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
    #[error("failed to write encoded profile: {0}")]
    EncodeIo(#[source] io::Error),
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
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct DynamicLocationIndex {
    value: u32,
}

impl DynamicLocationIndex {}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DynamicFunction {
    pub name: DynamicStringIndex,
    pub filename: DynamicStringIndex,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DynamicLocation {
    pub function: DynamicFunctionIndex,
    pub line: u32,
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

const _: () = {
    assert!(core::mem::size_of::<DynamicFunctionIndex>() == 4);
    assert!(core::mem::align_of::<DynamicFunctionIndex>() == 4);
    assert!(core::mem::size_of::<DynamicLocationIndex>() == 4);
    assert!(core::mem::align_of::<DynamicLocationIndex>() == 4);
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

type StoredLocation = DynamicLocation;

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

    fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.strings.iter()
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

struct DynamicMappedAllocation {
    ptr: NonNull<[u8]>,
    layout: Layout,
}

impl DynamicMappedAllocation {
    fn try_new(size: usize, align: usize) -> Result<Self, DynamicProfileError> {
        let layout = Layout::from_size_align(size, align)
            .map_err(|_| DynamicProfileError::StringTableFull)?
            .pad_to_align();
        let ptr = VirtualAllocator {}
            .allocate(layout)
            .map_err(|_| DynamicProfileError::StringTableFull)?;
        Ok(Self { ptr, layout })
    }

    fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr().cast::<u8>()
    }
}

impl Drop for DynamicMappedAllocation {
    fn drop(&mut self) {
        unsafe {
            VirtualAllocator {}.deallocate(self.ptr.cast(), self.layout);
        }
    }
}

unsafe impl Send for DynamicMappedAllocation {}
unsafe impl Sync for DynamicMappedAllocation {}

#[repr(C, align(64))]
struct CachelineAtomicU64 {
    value: AtomicU64,
    _padding: [u8; 56],
}

impl CachelineAtomicU64 {
    const fn new(value: u64) -> Self {
        Self {
            value: AtomicU64::new(value),
            _padding: [0; 56],
        }
    }
}

const _: () = {
    assert!(core::mem::size_of::<CachelineAtomicU64>() == 64);
    assert!(core::mem::align_of::<CachelineAtomicU64>() == 64);
};

#[derive(Clone, Copy)]
struct DynamicMappedBumpAllocator {
    base: NonNull<u8>,
    len: usize,
    cursor: NonNull<AtomicU32>,
}

impl DynamicMappedBumpAllocator {
    const fn new(base: NonNull<u8>, len: usize, cursor: NonNull<AtomicU32>) -> Self {
        Self { base, len, cursor }
    }
}

unsafe impl Allocator for DynamicMappedBumpAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let layout = layout.pad_to_align();
        let size = layout.size();
        if size == 0 {
            return Err(AllocError);
        }

        let cursor = unsafe { self.cursor.as_ref() };
        let mut current = cursor.load(Ordering::Relaxed);
        loop {
            let aligned = align_up(current as usize, layout.align());
            let end = aligned.checked_add(size).ok_or(AllocError)?;
            if end > self.len {
                return Err(AllocError);
            }
            let end_u32 = u32::try_from(end).map_err(|_| AllocError)?;
            match cursor.compare_exchange_weak(
                current,
                end_u32,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let ptr = unsafe { self.base.as_ptr().add(aligned) };
                    let ptr = NonNull::new(ptr).ok_or(AllocError)?;
                    return Ok(NonNull::slice_from_raw_parts(ptr, size));
                }
                Err(next) => current = next,
            }
        }
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate(layout)
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {}
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
struct PackedStringEntry(u64);

impl PackedStringEntry {
    fn new(offset: u32, len: u16, index: u32) -> Self {
        let word = u64::from(offset)
            | (u64::from(len) << PACKED_STRING_ENTRY_OFFSET_BITS)
            | (u64::from(index)
                << (PACKED_STRING_ENTRY_OFFSET_BITS + PACKED_STRING_ENTRY_LEN_BITS));
        Self(word)
    }

    fn offset(self) -> u32 {
        (self.0 & PACKED_STRING_ENTRY_OFFSET_MASK) as u32
    }

    fn len(self) -> u16 {
        ((self.0 >> PACKED_STRING_ENTRY_OFFSET_BITS) & PACKED_STRING_ENTRY_LEN_MASK) as u16
    }

    fn index(self) -> u32 {
        ((self.0 >> (PACKED_STRING_ENTRY_OFFSET_BITS + PACKED_STRING_ENTRY_LEN_BITS))
            & PACKED_STRING_ENTRY_INDEX_MASK) as u32
    }
}

#[derive(Copy, Clone)]
struct WellKnownPublicStrings {
    local_root_span_id: DynamicStringIndex,
    trace_endpoint: DynamicStringIndex,
    end_timestamp_ns: DynamicStringIndex,
}

#[repr(C)]
struct DynamicDictionaryControl {
    refcount: CachelineAtomicU64,
    string_count: AtomicU32,
    function_count: AtomicU32,
    string_arena_tail: AtomicU32,
    string_hash_cursor: AtomicU32,
    function_hash_cursor: AtomicU32,
    string_mutex: parking_lot::RawMutex,
    function_mutex: parking_lot::RawMutex,
    string_table: MaybeUninit<HashTable<PackedStringEntry, DynamicMappedBumpAllocator>>,
    function_table: MaybeUninit<HashTable<u64, DynamicMappedBumpAllocator>>,
}

const _: () = {
    assert!(core::mem::size_of::<DynamicDictionaryControl>() <= DYNAMIC_CONTROL_REGION_BYTES);
};

struct RawMutexGuard<'a>(&'a parking_lot::RawMutex);

impl<'a> RawMutexGuard<'a> {
    fn lock(mutex: &'a parking_lot::RawMutex) -> Self {
        mutex.lock();
        Self(mutex)
    }
}

impl Drop for RawMutexGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.0.unlock();
        }
    }
}

struct DynamicDictionarySegment {
    _mapping: DynamicMappedAllocation,
    control: NonNull<DynamicDictionaryControl>,
    string_offsets: NonNull<AtomicU32>,
    function_entries: NonNull<AtomicU64>,
    arena_base: NonNull<u8>,
    arena_offset: usize,
    arena_len: usize,
}

impl DynamicDictionarySegment {
    fn try_new() -> Result<Self, DynamicProfileError> {
        let page_size =
            libdd_alloc::os::page_size().map_err(|_| DynamicProfileError::StringTableFull)?;
        let control_offset = page_size;
        let string_hash_offset = control_offset + DYNAMIC_CONTROL_REGION_BYTES;
        let function_hash_offset = string_hash_offset + DYNAMIC_HASH_TABLE_REGION_BYTES;
        let string_offsets_offset = function_hash_offset + DYNAMIC_HASH_TABLE_REGION_BYTES;
        let function_entries_offset = align_up(
            string_offsets_offset + DYNAMIC_STRING_ARRAY_BYTES,
            core::mem::align_of::<AtomicU64>(),
        );
        let arena_offset = align_up(
            function_entries_offset + DYNAMIC_FUNCTION_ARRAY_BYTES,
            core::mem::align_of::<u64>(),
        );
        let trailing_guard_offset = DYNAMIC_DICTIONARY_SEGMENT_BYTES
            .checked_sub(page_size)
            .ok_or(DynamicProfileError::StringTableFull)?;
        if arena_offset > trailing_guard_offset {
            return Err(DynamicProfileError::StringTableFull);
        }
        let arena_len = trailing_guard_offset - arena_offset;
        let mapping =
            DynamicMappedAllocation::try_new(DYNAMIC_DICTIONARY_SEGMENT_BYTES, page_size)?;
        #[cfg(unix)]
        unsafe {
            let base = mapping.as_ptr();
            if libc::mprotect(base.cast(), page_size, libc::PROT_NONE) != 0 {
                return Err(DynamicProfileError::StringTableFull);
            }
            if libc::mprotect(
                base.add(trailing_guard_offset).cast(),
                page_size,
                libc::PROT_NONE,
            ) != 0
            {
                return Err(DynamicProfileError::StringTableFull);
            }
        }

        let base = mapping.as_ptr();
        let control =
            NonNull::new(unsafe { base.add(control_offset).cast::<DynamicDictionaryControl>() })
                .ok_or(DynamicProfileError::StringTableFull)?;
        let string_offsets =
            NonNull::new(unsafe { base.add(string_offsets_offset).cast::<AtomicU32>() })
                .ok_or(DynamicProfileError::StringTableFull)?;
        let function_entries =
            NonNull::new(unsafe { base.add(function_entries_offset).cast::<AtomicU64>() })
                .ok_or(DynamicProfileError::StringTableFull)?;
        let arena_base = NonNull::new(unsafe { base.add(arena_offset) })
            .ok_or(DynamicProfileError::StringTableFull)?;

        Self::initialize_tables(control, base, string_hash_offset, function_hash_offset)?;
        Ok(Self {
            _mapping: mapping,
            control,
            string_offsets,
            function_entries,
            arena_base,
            arena_offset,
            arena_len,
        })
    }

    fn control(&self) -> &DynamicDictionaryControl {
        unsafe { self.control.as_ref() }
    }

    unsafe fn string_table_ptr(
        control: NonNull<DynamicDictionaryControl>,
    ) -> *mut HashTable<PackedStringEntry, DynamicMappedBumpAllocator> {
        unsafe { (*control.as_ptr()).string_table.as_mut_ptr() }
    }

    unsafe fn function_table_ptr(
        control: NonNull<DynamicDictionaryControl>,
    ) -> *mut HashTable<u64, DynamicMappedBumpAllocator> {
        unsafe { (*control.as_ptr()).function_table.as_mut_ptr() }
    }

    fn initialize_tables(
        control: NonNull<DynamicDictionaryControl>,
        base: *mut u8,
        string_hash_offset: usize,
        function_hash_offset: usize,
    ) -> Result<(), DynamicProfileError> {
        unsafe {
            let control = control.as_ptr();
            (*control).refcount = CachelineAtomicU64::new(1);
            (*control).string_count = AtomicU32::new(0);
            (*control).function_count = AtomicU32::new(0);
            (*control).string_arena_tail = AtomicU32::new(0);
            (*control).string_hash_cursor = AtomicU32::new(0);
            (*control).function_hash_cursor = AtomicU32::new(0);
            (*control).string_mutex = parking_lot::RawMutex::INIT;
            (*control).function_mutex = parking_lot::RawMutex::INIT;
        }

        let string_hash_base = NonNull::new(unsafe { base.add(string_hash_offset) })
            .ok_or(DynamicProfileError::StringTableFull)?;
        let function_hash_base = NonNull::new(unsafe { base.add(function_hash_offset) })
            .ok_or(DynamicProfileError::FunctionTableFull)?;
        let control_ref = unsafe { control.as_ref() };
        let string_hash_cursor = NonNull::from(&control_ref.string_hash_cursor);
        let function_hash_cursor = NonNull::from(&control_ref.function_hash_cursor);

        let string_alloc = DynamicMappedBumpAllocator::new(
            string_hash_base,
            DYNAMIC_HASH_TABLE_REGION_BYTES,
            string_hash_cursor,
        );
        let function_alloc = DynamicMappedBumpAllocator::new(
            function_hash_base,
            DYNAMIC_HASH_TABLE_REGION_BYTES,
            function_hash_cursor,
        );

        let mut string_table = HashTable::new_in(string_alloc);
        string_table
            .try_reserve(MAX_DYNAMIC_STRING_INDEX, |_| unreachable!())
            .map_err(|_| DynamicProfileError::StringTableFull)?;
        let mut function_table = HashTable::new_in(function_alloc);
        function_table
            .try_reserve(MAX_DYNAMIC_FUNCTION_INDEX, |_| unreachable!())
            .map_err(|_| DynamicProfileError::FunctionTableFull)?;

        unsafe {
            let control = control.as_ptr();
            (*control).string_table = MaybeUninit::new(string_table);
            (*control).function_table = MaybeUninit::new(function_table);
        }
        Ok(())
    }

    fn string_count(&self) -> u32 {
        self.control().string_count.load(Ordering::Acquire)
    }

    fn function_count(&self) -> u32 {
        self.control().function_count.load(Ordering::Acquire)
    }

    fn len(&self) -> usize {
        self.string_count() as usize + 1
    }

    fn get_string(&self, index: DynamicStringIndex) -> Option<&str> {
        if index.is_empty() {
            return Some("");
        }
        if index.value > self.string_count() {
            return None;
        }
        unsafe { Some(self.get_string_unchecked(index)) }
    }

    unsafe fn get_string_unchecked(&self, index: DynamicStringIndex) -> &str {
        if index.is_empty() {
            return "";
        }
        let offset = self.offset_slot(index.value).load(Ordering::Relaxed);
        unsafe { self.string_at_offset(offset) }
    }

    fn get_function(&self, index: DynamicFunctionIndex) -> Option<DynamicFunction> {
        if index.is_empty() {
            return Some(DynamicFunction::default());
        }
        if index.value > self.function_count() {
            return None;
        }
        let packed = self.function_slot(index.value).load(Ordering::Acquire);
        Some(unpack_function_value(packed))
    }

    fn iter_strings(&self) -> DynamicStringIter<'_> {
        DynamicStringIter {
            segment: self,
            next: 0,
            end: self.string_count(),
        }
    }

    fn iter_functions_non_empty(&self) -> impl Iterator<Item = (u32, DynamicFunction)> + '_ {
        let end = self.function_count();
        (1..=end).map(|index| {
            let packed = self.function_slot(index).load(Ordering::Acquire);
            (index, unpack_function_value(packed))
        })
    }

    fn try_insert_string(
        &self,
        hash_builder: &BuildHasherDefault<rustc_hash::FxHasher>,
        s: &str,
    ) -> Result<DynamicStringIndex, DynamicProfileError> {
        if s.is_empty() {
            return Ok(DynamicStringIndex::EMPTY);
        }
        if s.len() > MAX_DYNAMIC_STRING_LENGTH {
            return Err(DynamicProfileError::StringTooLong);
        }

        let hash = hash_builder.hash_one(s);
        let _guard = RawMutexGuard::lock(&self.control().string_mutex);
        let table = unsafe { &mut *Self::string_table_ptr(self.control) };
        if let Some(found) = table.find(hash, |entry| self.entry_matches(*entry, s)) {
            return Ok(DynamicStringIndex {
                value: found.index(),
            });
        }

        let next_index = self
            .string_count()
            .checked_add(1)
            .ok_or(DynamicProfileError::StringTableFull)?;
        if next_index as usize > MAX_DYNAMIC_STRING_INDEX {
            return Err(DynamicProfileError::StringTableFull);
        }
        let offset = self.allocate_string_record(hash, s)?;
        let entry = PackedStringEntry::new(offset, s.len() as u16, next_index);
        table.insert_unique(hash, entry, |existing| {
            self.hash_at_offset(existing.offset())
        });
        self.offset_slot(next_index)
            .store(offset, Ordering::Relaxed);
        self.control()
            .string_count
            .store(next_index, Ordering::Release);
        Ok(DynamicStringIndex { value: next_index })
    }

    fn try_insert_function(
        &self,
        hash_builder: &BuildHasherDefault<rustc_hash::FxHasher>,
        name: DynamicStringIndex,
        filename: DynamicStringIndex,
    ) -> Result<DynamicFunctionIndex, DynamicProfileError> {
        let key = function_lookup_key(name.value, filename.value);
        let hash = hash_builder.hash_one(key);
        let _guard = RawMutexGuard::lock(&self.control().function_mutex);
        let table = unsafe { &mut *Self::function_table_ptr(self.control) };
        if let Some(found) = table.find(hash, |word| function_lookup_key_from_packed(*word) == key)
        {
            return Ok(DynamicFunctionIndex {
                value: unpack_function_index(*found),
            });
        }

        let next_index = self
            .function_count()
            .checked_add(1)
            .ok_or(DynamicProfileError::FunctionTableFull)?;
        if next_index as usize > MAX_DYNAMIC_FUNCTION_INDEX {
            return Err(DynamicProfileError::FunctionTableFull);
        }
        let packed = pack_function_value(name.value, filename.value, next_index)?;
        table.insert_unique(hash, packed, |existing| {
            hash_builder.hash_one(function_lookup_key_from_packed(*existing))
        });
        self.function_slot(next_index)
            .store(packed, Ordering::Release);
        self.control()
            .function_count
            .store(next_index, Ordering::Release);
        Ok(DynamicFunctionIndex { value: next_index })
    }

    fn entry_matches(&self, entry: PackedStringEntry, s: &str) -> bool {
        usize::from(entry.len()) == s.len() && self.bytes_at_offset(entry.offset()) == s.as_bytes()
    }

    fn hash_at_offset(&self, offset: u32) -> u64 {
        let ptr = unsafe { self._mapping.as_ptr().add(offset as usize) };
        unsafe { ptr.cast::<u64>().read_unaligned() }
    }

    fn bytes_at_offset(&self, offset: u32) -> &[u8] {
        let ptr = unsafe { self._mapping.as_ptr().add(offset as usize) };
        let len = unsafe {
            ptr.add(core::mem::size_of::<u64>())
                .cast::<u16>()
                .read_unaligned()
        };
        unsafe {
            slice::from_raw_parts(
                ptr.add(DYNAMIC_STRING_RECORD_HEADER_BYTES),
                usize::from(len),
            )
        }
    }

    unsafe fn string_at_offset(&self, offset: u32) -> &str {
        unsafe { str::from_utf8_unchecked(self.bytes_at_offset(offset)) }
    }

    fn allocate_string_record(&self, hash: u64, s: &str) -> Result<u32, DynamicProfileError> {
        let record_len = DYNAMIC_STRING_RECORD_HEADER_BYTES
            .checked_add(s.len())
            .ok_or(DynamicProfileError::StringTableFull)?;
        let tail = self.control().string_arena_tail.load(Ordering::Relaxed) as usize;
        let end = tail
            .checked_add(record_len)
            .ok_or(DynamicProfileError::StringTableFull)?;
        if end > self.arena_len {
            return Err(DynamicProfileError::StringTableFull);
        }
        self.control().string_arena_tail.store(
            u32::try_from(end).map_err(|_| DynamicProfileError::StringTableFull)?,
            Ordering::Relaxed,
        );
        let offset = self
            .arena_offset
            .checked_add(tail)
            .ok_or(DynamicProfileError::StringTableFull)?;
        let offset_u32 = u32::try_from(offset).map_err(|_| DynamicProfileError::StringTableFull)?;
        let ptr = unsafe { self.arena_base.as_ptr().add(tail) };
        unsafe {
            ptr.cast::<u64>().write_unaligned(hash);
            ptr.add(core::mem::size_of::<u64>())
                .cast::<u16>()
                .write_unaligned(s.len() as u16);
            ptr.add(DYNAMIC_STRING_RECORD_HEADER_BYTES)
                .copy_from_nonoverlapping(s.as_ptr(), s.len());
        }
        Ok(offset_u32)
    }

    fn offset_slot(&self, index: u32) -> &AtomicU32 {
        unsafe { &*self.string_offsets.as_ptr().add(index as usize) }
    }

    fn function_slot(&self, index: u32) -> &AtomicU64 {
        unsafe { &*self.function_entries.as_ptr().add(index as usize) }
    }
}

impl Drop for DynamicDictionarySegment {
    fn drop(&mut self) {
        unsafe {
            core::ptr::drop_in_place((*self.control.as_ptr()).string_table.as_mut_ptr());
            core::ptr::drop_in_place((*self.control.as_ptr()).function_table.as_mut_ptr());
        }
    }
}

struct DynamicStringIter<'a> {
    segment: &'a DynamicDictionarySegment,
    next: u32,
    end: u32,
}

impl<'a> Iterator for DynamicStringIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == 0 {
            self.next = 1;
            return Some("");
        }
        if self.next > self.end {
            return None;
        }
        let index = DynamicStringIndex { value: self.next };
        self.next += 1;
        unsafe { Some(self.segment.get_string_unchecked(index)) }
    }
}

pub struct DynamicProfilesDictionary {
    segment: DynamicDictionarySegment,
    hasher: BuildHasherDefault<rustc_hash::FxHasher>,
    well_known: WellKnownPublicStrings,
}

impl DynamicProfilesDictionary {
    pub fn try_new() -> Result<Self, DynamicProfileError> {
        let segment = DynamicDictionarySegment::try_new()?;
        let hasher = BuildHasherDefault::default();
        let mut dictionary = Self {
            segment,
            hasher,
            well_known: WellKnownPublicStrings {
                local_root_span_id: DynamicStringIndex::EMPTY,
                trace_endpoint: DynamicStringIndex::EMPTY,
                end_timestamp_ns: DynamicStringIndex::EMPTY,
            },
        };
        let local_root_span_id = dictionary.try_insert_str("local root span id")?;
        let trace_endpoint = dictionary.try_insert_str("trace endpoint")?;
        let end_timestamp_ns = dictionary.try_insert_str("end_timestamp_ns")?;
        dictionary.well_known = WellKnownPublicStrings {
            local_root_span_id,
            trace_endpoint,
            end_timestamp_ns,
        };
        Ok(dictionary)
    }

    pub fn try_insert_str(&self, s: &str) -> Result<DynamicStringIndex, DynamicProfileError> {
        self.segment.try_insert_string(&self.hasher, s)
    }

    pub fn try_insert_function(
        &self,
        function: DynamicFunction,
    ) -> Result<DynamicFunctionIndex, DynamicProfileError> {
        self.ensure_public_string(function.name)?;
        self.ensure_public_string(function.filename)?;
        self.segment
            .try_insert_function(&self.hasher, function.name, function.filename)
    }

    /// # Safety
    ///
    /// The caller must ensure `id` is valid for this dictionary and that the
    /// returned string is not used after the dictionary is dropped.
    pub unsafe fn get_str(&self, id: DynamicStringIndex) -> &str {
        unsafe { self.segment.get_string_unchecked(id) }
    }

    /// # Safety
    ///
    /// The caller must ensure `id` is valid for this dictionary and that the
    /// dictionary remains alive for the duration of the call.
    pub unsafe fn get_func(&self, id: DynamicFunctionIndex) -> DynamicFunction {
        self.segment.get_function(id).unwrap_or_default()
    }

    fn iter_strings(&self) -> DynamicStringIter<'_> {
        self.segment.iter_strings()
    }

    fn iter_functions_non_empty(&self) -> impl Iterator<Item = (u32, DynamicFunction)> + '_ {
        self.segment.iter_functions_non_empty()
    }

    fn len(&self) -> usize {
        self.segment.len()
    }

    fn contains_string(&self, index: DynamicStringIndex) -> bool {
        self.segment.get_string(index).is_some()
    }

    fn contains_function(&self, index: DynamicFunctionIndex) -> bool {
        self.segment.get_function(index).is_some()
    }

    fn ensure_public_string(&self, index: DynamicStringIndex) -> Result<(), DynamicProfileError> {
        if self.contains_string(index) {
            Ok(())
        } else {
            Err(DynamicProfileError::InvalidStringIndex { index: index.value })
        }
    }

    fn ensure_profile_strings(
        &self,
        sample_types: &[api::SampleType],
        period: Option<api::Period>,
    ) -> Result<(), DynamicProfileError> {
        for sample_type in sample_types.iter().copied() {
            let value_type: api::ValueType<'static> = sample_type.into();
            self.try_insert_str(value_type.r#type)?;
            self.try_insert_str(value_type.unit)?;
        }
        if let Some(period) = period {
            let value_type: api::ValueType<'static> = period.sample_type.into();
            self.try_insert_str(value_type.r#type)?;
            self.try_insert_str(value_type.unit)?;
        }
        Ok(())
    }

    fn well_known(&self) -> WellKnownPublicStrings {
        self.well_known
    }
}

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

fn function_lookup_key(name: u32, filename: u32) -> u64 {
    u64::from(name) | (u64::from(filename) << PACKED_FUNCTION_NAME_BITS)
}

fn function_lookup_key_from_packed(packed: u64) -> u64 {
    function_lookup_key(
        (packed & PACKED_FUNCTION_NAME_MASK) as u32,
        ((packed >> PACKED_FUNCTION_NAME_BITS) & PACKED_FUNCTION_FILENAME_MASK) as u32,
    )
}

fn pack_function_value(name: u32, filename: u32, index: u32) -> Result<u64, DynamicProfileError> {
    if name as usize > MAX_DYNAMIC_STRING_INDEX || filename as usize > MAX_DYNAMIC_STRING_INDEX {
        return Err(DynamicProfileError::InvalidStringIndex {
            index: name.max(filename),
        });
    }
    if index as usize > MAX_DYNAMIC_FUNCTION_INDEX {
        return Err(DynamicProfileError::FunctionTableFull);
    }
    Ok(u64::from(name)
        | (u64::from(filename) << PACKED_FUNCTION_NAME_BITS)
        | (u64::from(index) << (PACKED_FUNCTION_NAME_BITS + PACKED_FUNCTION_FILENAME_BITS)))
}

fn unpack_function_index(packed: u64) -> u32 {
    ((packed >> (PACKED_FUNCTION_NAME_BITS + PACKED_FUNCTION_FILENAME_BITS))
        & PACKED_FUNCTION_INDEX_MASK) as u32
}

fn unpack_function_value(packed: u64) -> DynamicFunction {
    DynamicFunction {
        name: DynamicStringIndex {
            value: (packed & PACKED_FUNCTION_NAME_MASK) as u32,
        },
        filename: DynamicStringIndex {
            value: ((packed >> PACKED_FUNCTION_NAME_BITS) & PACKED_FUNCTION_FILENAME_MASK) as u32,
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StoredStackTrace {
    packed_location_ids: &'static [u64],
    location_ids_len: u32,
}

impl StoredStackTrace {
    const EMPTY: Self = Self {
        packed_location_ids: &[],
        location_ids_len: 0,
    };

    #[inline]
    fn location_ids_len(&self) -> usize {
        self.location_ids_len as usize
    }

    #[inline]
    fn packed_location_ids_len(len: usize) -> usize {
        len.div_ceil(PACKED_LOCATION_IDS_PER_WORD)
    }

    fn extend_location_ids_u64(&self, out: &mut Vec<u64>) {
        let mut remaining = self.location_ids_len();
        for packed in self.packed_location_ids {
            let chunk_len = remaining.min(PACKED_LOCATION_IDS_PER_WORD);
            let mut word = *packed;
            for _ in 0..chunk_len {
                out.push(word & PACKED_LOCATION_ID_MASK);
                word >>= PACKED_LOCATION_ID_BITS;
            }
            remaining -= chunk_len;
        }
    }
}

impl std::hash::Hash for StoredStackTrace {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.location_ids_len);
        for packed in self.packed_location_ids {
            state.write_u64(*packed);
        }
    }
}

#[allow(clippy::mut_from_ref)]
fn try_allocate_arena_slice<T: Copy>(
    arena: &ChainAllocator<VirtualAllocator>,
    len: usize,
) -> Result<&mut [T], ()> {
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

fn allocate_owned_locations(
    arena: &ChainAllocator<VirtualAllocator>,
    locations: &[DynamicLocation],
) -> Result<&'static [DynamicLocation], DynamicProfileError> {
    let owned = try_allocate_arena_slice::<DynamicLocation>(arena, locations.len())
        .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
    owned.copy_from_slice(locations);
    Ok(unsafe { core::mem::transmute::<&[DynamicLocation], &'static [DynamicLocation]>(owned) })
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

struct DynamicLocationTable {
    entries: FxIndexSet<StoredLocation>,
}

impl DynamicLocationTable {
    fn new() -> Self {
        let mut entries = FxIndexSet::default();
        entries.reserve(112);
        entries.insert(StoredLocation::default());
        Self { entries }
    }

    fn iter_non_empty(&self) -> impl Iterator<Item = (u32, &StoredLocation)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .map(|(offset, item)| (offset as u32, item))
    }

    fn intern(
        &mut self,
        location: DynamicLocation,
    ) -> Result<DynamicLocationIndex, DynamicProfileError> {
        if self.entries.len() > MAX_DYNAMIC_LOCATION_INDEX {
            return Err(DynamicProfileError::StackTraceTableFull);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        let (id, _) = self.entries.insert_full(location);
        let id = u32::try_from(id).map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        Ok(DynamicLocationIndex { value: id })
    }
}

fn pack_and_intern_locations(
    arena: &ChainAllocator<VirtualAllocator>,
    location_table: &mut DynamicLocationTable,
    locations: &[DynamicLocation],
) -> Result<StoredStackTrace, DynamicProfileError> {
    let location_ids_len =
        u32::try_from(locations.len()).map_err(|_| DynamicProfileError::StackTraceTableFull)?;
    let packed_len = StoredStackTrace::packed_location_ids_len(locations.len());
    let packed_location_ids = try_allocate_arena_slice::<u64>(arena, packed_len)
        .map_err(|_| DynamicProfileError::StackTraceTableFull)?;

    let mut chunks = locations.chunks_exact(PACKED_LOCATION_IDS_PER_WORD);
    for (packed, chunk) in packed_location_ids.iter_mut().zip(&mut chunks) {
        let location_id0 = location_table.intern(chunk[0])?;
        let location_id1 = location_table.intern(chunk[1])?;
        let location_id2 = location_table.intern(chunk[2])?;
        *packed = u64::from(location_id0.value)
            | (u64::from(location_id1.value) << PACKED_LOCATION_ID_BITS)
            | (u64::from(location_id2.value) << (2 * PACKED_LOCATION_ID_BITS));
    }

    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let packed = packed_location_ids
            .last_mut()
            .ok_or(DynamicProfileError::StackTraceTableFull)?;
        let mut word = 0_u64;
        for (offset, location) in remainder.iter().enumerate() {
            let location_id = location_table.intern(*location)?;
            word |= u64::from(location_id.value) << (offset * PACKED_LOCATION_ID_BITS);
        }
        *packed = word;
    }

    Ok(StoredStackTrace {
        packed_location_ids: unsafe {
            core::mem::transmute::<&[u64], &'static [u64]>(packed_location_ids)
        },
        location_ids_len,
    })
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
        entries.insert(StoredStackTrace::EMPTY);
        Self {
            arena,
            cache,
            entries,
        }
    }
    fn get(&self, index: DynamicStackTraceIndex) -> Option<&StoredStackTrace> {
        self.entries.get_index(index.value as usize)
    }

    fn get_index_by_locations(
        &mut self,
        locations: &[DynamicLocation],
    ) -> Option<DynamicStackTraceIndex> {
        let locations = DynamicLocationSlice(locations);
        let hash = self.cache.hasher().hash_one(locations);
        match self
            .cache
            .raw_entry_mut_v1()
            .from_hash(hash, |stored| *stored == locations)
        {
            RawEntryMut::Occupied(entry) => Some(DynamicStackTraceIndex {
                value: *entry.get(),
            }),
            RawEntryMut::Vacant(_) => None,
        }
    }

    fn intern(
        &mut self,
        locations: &[DynamicLocation],
        location_table: &mut DynamicLocationTable,
    ) -> Result<DynamicStackTraceIndex, DynamicProfileError> {
        self.cache
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        if self.entries.len() >= u32::MAX as usize {
            return Err(DynamicProfileError::StackTraceTableFull);
        }
        let stacktrace = pack_and_intern_locations(&self.arena, location_table, locations)?;
        self.entries
            .try_reserve(1)
            .map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        let (id, _) = self.entries.insert_full(stacktrace);
        let id = u32::try_from(id).map_err(|_| DynamicProfileError::StackTraceTableFull)?;
        let owned_locations = allocate_owned_locations(&self.arena, locations)?;
        self.cache.insert(DynamicLocationSlice(owned_locations), id);
        Ok(DynamicStackTraceIndex { value: id })
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
        let mut entries: Vec<&'static [StoredLabel]> = Vec::with_capacity(28);
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
    locations: DynamicLocationTable,
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
            locations: DynamicLocationTable::new(),
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

#[derive(Default)]
struct EncodeScratch {
    stored_labels: Vec<StoredLabel>,
    protobuf_labels: Vec<protobuf::Label>,
    location_ids: Vec<u64>,
    values: Vec<i64>,
}

impl EncodeScratch {
    fn prepare_stored_labels(
        &mut self,
        labels: &[StoredLabel],
        extra_labels: usize,
    ) -> Result<(), DynamicProfileError> {
        self.stored_labels.clear();
        self.stored_labels
            .try_reserve(labels.len().saturating_add(extra_labels))
            .map_err(|_| DynamicProfileError::SerializationScratchAllocation)?;
        self.stored_labels.extend_from_slice(labels);
        Ok(())
    }

    fn prepare_protobuf_labels(&mut self, len: usize) -> Result<(), DynamicProfileError> {
        self.protobuf_labels.clear();
        self.protobuf_labels
            .try_reserve(len)
            .map_err(|_| DynamicProfileError::SerializationScratchAllocation)?;
        Ok(())
    }

    fn prepare_location_ids(&mut self, len: usize) -> Result<(), DynamicProfileError> {
        self.location_ids.clear();
        self.location_ids
            .try_reserve(len)
            .map_err(|_| DynamicProfileError::SerializationScratchAllocation)?;
        Ok(())
    }

    fn prepare_values(&mut self, values: &[i64]) -> Result<(), DynamicProfileError> {
        self.values.clear();
        self.values
            .try_reserve(values.len())
            .map_err(|_| DynamicProfileError::SerializationScratchAllocation)?;
        self.values.extend_from_slice(values);
        Ok(())
    }
}

pub struct DynamicProfile {
    sample_types: Box<[api::SampleType]>,
    period: Option<api::Period>,
    start_time: SystemTime,
    dictionary: ProfilesArc<DynamicProfilesDictionary>,
    period_local: PeriodLocalData,
    well_known: WellKnownPublicStrings,
}

impl DynamicProfile {
    #[cfg(test)]
    pub fn try_new(
        sample_types: &[api::SampleType],
        period: Option<api::Period>,
        start_time: Option<SystemTime>,
    ) -> Result<Self, DynamicProfileError> {
        let dictionary = ProfilesArc::try_new(DynamicProfilesDictionary::try_new()?)
            .map_err(|_| DynamicProfileError::StringTableFull)?;
        Self::try_new_with_dictionary(sample_types, period, start_time, dictionary)
    }

    pub fn try_new_with_dictionary(
        sample_types: &[api::SampleType],
        period: Option<api::Period>,
        start_time: Option<SystemTime>,
        dictionary: ProfilesArc<DynamicProfilesDictionary>,
    ) -> Result<Self, DynamicProfileError> {
        dictionary.ensure_profile_strings(sample_types, period)?;
        let well_known = dictionary.well_known();
        Ok(Self {
            sample_types: sample_types.to_vec().into_boxed_slice(),
            period,
            start_time: start_time.unwrap_or_else(SystemTime::now),
            dictionary,
            period_local: PeriodLocalData::try_new(sample_types.len())?,
            well_known,
        })
    }

    pub fn intern_string(&self, s: &str) -> Result<DynamicStringIndex, DynamicProfileError> {
        self.dictionary.try_insert_str(s)
    }

    pub fn intern_function(
        &self,
        name: DynamicStringIndex,
        filename: DynamicStringIndex,
    ) -> Result<DynamicFunctionIndex, DynamicProfileError> {
        self.dictionary
            .try_insert_function(DynamicFunction { name, filename })
    }

    pub fn intern_stacktrace(
        &mut self,
        locations: &[DynamicLocation],
    ) -> Result<DynamicStackTraceIndex, DynamicProfileError> {
        self.validate_locations(locations)?;
        let stacktraces = &mut self.period_local.stacktraces;
        if let Some(stacktrace) = stacktraces.get_index_by_locations(locations) {
            return Ok(stacktrace);
        }
        let location_table = &mut self.period_local.locations;
        stacktraces.intern(locations, location_table)
    }

    pub fn add_sample_by_stacktrace(
        &mut self,
        stacktrace: DynamicStackTraceIndex,
        sample: DynamicSample<'_>,
        timestamp_ns: i64,
    ) -> Result<(), DynamicProfileError> {
        #[cfg(debug_assertions)]
        self.validate_sample_labels(sample.labels)?;
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
        #[cfg(debug_assertions)]
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
        #[cfg(debug_assertions)]
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
        #[cfg(debug_assertions)]
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
        const INITIAL_PPROF_BUFFER_SIZE: usize = 32 * 1024;
        const MAX_PROFILE_SIZE: usize = 16 * 1024 * 1024;

        let mut compressor = Compressor::<DefaultProfileCodec>::try_new(
            INITIAL_PPROF_BUFFER_SIZE,
            MAX_PROFILE_SIZE,
            NativeProfile::COMPRESSION_LEVEL,
        )
        .map_err(DynamicProfileError::Compression)?;
        let mut encoded = self.encode(&mut compressor, end_time, duration)?;
        encoded.buffer = compressor
            .finish()
            .map_err(DynamicProfileError::Compression)?;
        self.clear_period_local_data()?;
        Ok(encoded)
    }

    pub fn clear_period_local_data(&mut self) -> Result<(), DynamicProfileError> {
        self.period_local = PeriodLocalData::try_new(self.sample_types.len())?;
        self.start_time = SystemTime::now();
        Ok(())
    }

    pub fn clear_all_data(&mut self) -> Result<(), DynamicProfileError> {
        self.period_local = PeriodLocalData::try_new(self.sample_types.len())?;
        self.start_time = SystemTime::now();
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn ensure_public_string(&self, index: DynamicStringIndex) -> Result<(), DynamicProfileError> {
        self.dictionary.ensure_public_string(index)
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
            if !self.dictionary.contains_function(location.function) {
                return Err(DynamicProfileError::InvalidFunctionIndex {
                    index: location.function.value,
                });
            }
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn validate_sample_labels(
        &self,
        labels: &[DynamicLabel<'_>],
    ) -> Result<(), DynamicProfileError> {
        if !labels_have_unique_keys(labels) {
            return Err(DynamicProfileError::DuplicateLabelKey);
        }

        for label in labels {
            if label.key == self.well_known.local_root_span_id
                && (!label.str.is_empty() || label.num == 0)
            {
                return Err(DynamicProfileError::InvalidLabelValue);
            }

            if label.key == self.well_known.end_timestamp_ns {
                return Err(DynamicProfileError::InvalidLabelValue);
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
            #[cfg(debug_assertions)]
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
    ) -> Result<protobuf::ValueType, DynamicProfileError> {
        let value_type: api::ValueType<'static> = sample_type.into();
        let ty = self.dictionary.try_insert_str(value_type.r#type)?;
        let unit = self.dictionary.try_insert_str(value_type.unit)?;
        Ok(protobuf::ValueType {
            r#type: Record::from(StringOffset::from(ty.value)),
            unit: Record::from(StringOffset::from(unit.value)),
        })
    }

    fn private_string_to_offset(
        &self,
        private_id: u32,
    ) -> Result<StringOffset, DynamicProfileError> {
        if private_id == 0 {
            Ok(StringOffset::ZERO)
        } else {
            let offset = self.dictionary.len() + (private_id as usize) - 1;
            StringOffset::try_from(offset).map_err(|_| DynamicProfileError::StringTableFull)
        }
    }

    fn encode<W: io::Write>(
        &mut self,
        writer: &mut W,
        end_time: Option<SystemTime>,
        duration: Option<Duration>,
    ) -> Result<EncodedProfile, DynamicProfileError> {
        let end = end_time.unwrap_or_else(SystemTime::now);
        let start = self.start_time;
        let endpoints_stats = self.period_local.endpoint_stats.clone();
        let duration_nanos = duration
            .unwrap_or_else(|| end.duration_since(start).unwrap_or(Duration::ZERO))
            .as_nanos()
            .min(i64::MAX as u128) as i64;

        let sample_types = self
            .sample_types
            .iter()
            .copied()
            .map(|sample_type| self.public_value_type(sample_type))
            .collect::<Result<Vec<protobuf::ValueType>, _>>()?;
        let period_type = self
            .period
            .map(|period| self.public_value_type(period.sample_type))
            .transpose()?;
        let period = self.period.map_or(0, |period| period.value);
        let mut scratch = EncodeScratch::default();

        let empty_timestamped =
            DynamicCompressedTimestampedSamples::try_new(self.sample_types.len())
                .map_err(DynamicProfileError::TimestampedObservationIo)?;
        let timestamped = std::mem::replace(&mut self.period_local.timestamped, empty_timestamped);
        let timestamped_iter = timestamped
            .try_into_iter()
            .map_err(DynamicProfileError::TimestampedObservationIo)?;
        for item in timestamped_iter {
            let (stacktrace, labels, timestamp_delta_ns, values) =
                item.map_err(DynamicProfileError::TimestampedObservationIo)?;
            let key = SampleKey { stacktrace, labels };
            self.encode_sample(writer, &mut scratch, &key, timestamp_delta_ns, &values)?;
        }
        for (key, values) in &self.period_local.aggregated {
            self.encode_sample(writer, &mut scratch, key, 0, values)?;
        }

        for sample_type in sample_types {
            Record::<_, 1, NO_OPT_ZERO>::from(sample_type)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }

        for (location_id, location) in self.period_local.locations.iter_non_empty() {
            let location = protobuf::Location {
                id: Record::from(u64::from(location_id)),
                mapping_id: Record::from(0_u64),
                address: Record::from(0_u64),
                line: Record::from(protobuf::Line {
                    function_id: Record::from(u64::from(location.function.value)),
                    lineno: Record::from(i64::from(location.line)),
                }),
            };
            Record::<_, 4, NO_OPT_ZERO>::from(location)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }

        for (id, function) in self.dictionary.iter_functions_non_empty() {
            let function = protobuf::Function {
                id: Record::from(u64::from(id)),
                name: Record::from(StringOffset::from(function.name.value)),
                system_name: Record::from(StringOffset::ZERO),
                filename: Record::from(StringOffset::from(function.filename.value)),
            };
            Record::<_, 5, NO_OPT_ZERO>::from(function)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }

        for item in self.dictionary.iter_strings() {
            Record::<_, 6, NO_OPT_ZERO>::from(item)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }
        for item in self.period_local.private_strings.iter().skip(1) {
            Record::<_, 6, NO_OPT_ZERO>::from(item)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }

        let time_nanos = start
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_nanos().min(i64::MAX as u128) as i64
            });

        Record::<_, 9, OPT_ZERO>::from(time_nanos)
            .encode(writer)
            .map_err(DynamicProfileError::EncodeIo)?;
        Record::<_, 10, OPT_ZERO>::from(duration_nanos)
            .encode(writer)
            .map_err(DynamicProfileError::EncodeIo)?;
        if let Some(period_type) = period_type {
            Record::<_, 11, OPT_ZERO>::from(period_type)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
            Record::<_, 12, OPT_ZERO>::from(period)
                .encode(writer)
                .map_err(DynamicProfileError::EncodeIo)?;
        }

        Ok(EncodedProfile {
            start,
            end,
            buffer: Vec::new(),
            endpoints_stats,
        })
    }

    fn encode_sample<W: io::Write>(
        &self,
        writer: &mut W,
        scratch: &mut EncodeScratch,
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
        let labels = self
            .period_local
            .label_sets
            .get(key.labels)
            .ok_or(DynamicProfileError::InvalidLabelSetIndex { index: key.labels })?;
        let extra_labels =
            usize::from(self.endpoint_for_labels(labels).is_some()) + usize::from(timestamp != 0);
        scratch.prepare_stored_labels(labels, extra_labels)?;
        if let Some(endpoint_id) = self.endpoint_for_labels(&scratch.stored_labels) {
            scratch.stored_labels.push(StoredLabel {
                key: self.well_known.trace_endpoint.value,
                str: endpoint_id,
                num: 0,
            });
        }
        scratch.prepare_values(values)?;
        self.period_local
            .upscaling_rules
            .upscale_values(&mut scratch.values, &scratch.stored_labels);
        if timestamp != 0 {
            scratch.stored_labels.push(StoredLabel {
                key: self.well_known.end_timestamp_ns.value,
                str: 0,
                num: timestamp,
            });
        }
        scratch.prepare_protobuf_labels(scratch.stored_labels.len())?;
        for label in &scratch.stored_labels {
            scratch
                .protobuf_labels
                .push(self.to_protobuf_label(*label)?);
        }
        scratch.prepare_location_ids(stacktrace.location_ids_len())?;
        stacktrace.extend_location_ids_u64(&mut scratch.location_ids);
        let sample = protobuf::Sample {
            location_ids: Record::from(scratch.location_ids.as_slice()),
            values: Record::from(scratch.values.as_slice()),
            labels: unsafe {
                &*(scratch.protobuf_labels.as_slice() as *const [protobuf::Label]
                    as *const [Record<protobuf::Label, 3, NO_OPT_ZERO>])
            },
        };
        Record::<_, 2, NO_OPT_ZERO>::from(sample)
            .encode(writer)
            .map_err(DynamicProfileError::EncodeIo)?;
        Ok(())
    }

    fn to_protobuf_label(
        &self,
        label: StoredLabel,
    ) -> Result<protobuf::Label, DynamicProfileError> {
        if label.str != 0 {
            Ok(protobuf::Label {
                key: Record::from(StringOffset::from(label.key)),
                str: Record::from(self.private_string_to_offset(label.str)?),
                num: Record::from(0_i64),
                num_unit: Record::from(StringOffset::ZERO),
            })
        } else {
            Ok(protobuf::Label {
                key: Record::from(StringOffset::from(label.key)),
                str: Record::from(StringOffset::ZERO),
                num: Record::from(label.num),
                num_unit: Record::from(StringOffset::ZERO),
            })
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
    use crate::pprof::test_utils::roundtrip_to_pprof;
    use libdd_profiling_protobuf::prost_impls::{self as pprof, Message};
    use std::borrow::Cow;
    use std::collections::{HashMap, HashSet};
    use std::mem::align_of;

    #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
    struct ExportedFrame {
        function: String,
        line: i64,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
    struct ExportedLabel {
        key: String,
        str_value: Option<String>,
        num: i64,
        num_unit: Option<String>,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
    struct ExportedSample {
        locations: Vec<ExportedFrame>,
        values: Vec<i64>,
        labels: Vec<ExportedLabel>,
    }

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

    fn exported_samples(profile: &pprof::Profile) -> Vec<ExportedSample> {
        let function_names: HashMap<_, _> = profile
            .functions
            .iter()
            .map(|function| {
                let name = profile
                    .string_table
                    .get(function.name as usize)
                    .expect("function name to exist")
                    .clone();
                (function.id, name)
            })
            .collect();
        let locations: HashMap<_, _> = profile
            .locations
            .iter()
            .map(|location| (location.id, location))
            .collect();

        let mut samples = profile
            .samples
            .iter()
            .map(|sample| {
                let mut labels = sample
                    .labels
                    .iter()
                    .map(|label| ExportedLabel {
                        key: profile
                            .string_table
                            .get(label.key as usize)
                            .expect("label key to exist")
                            .clone(),
                        str_value: (label.str != 0).then(|| {
                            profile
                                .string_table
                                .get(label.str as usize)
                                .expect("label string to exist")
                                .clone()
                        }),
                        num: label.num,
                        num_unit: (label.num_unit != 0).then(|| {
                            profile
                                .string_table
                                .get(label.num_unit as usize)
                                .expect("label unit to exist")
                                .clone()
                        }),
                    })
                    .collect::<Vec<_>>();
                labels.sort();

                let locations = sample
                    .location_ids
                    .iter()
                    .map(|location_id| {
                        let location = locations
                            .get(location_id)
                            .expect("location id to exist in profile");
                        let line = location
                            .lines
                            .first()
                            .expect("dynamic location to have a line");
                        ExportedFrame {
                            function: function_names
                                .get(&line.function_id)
                                .expect("function id to exist")
                                .clone(),
                            line: line.line,
                        }
                    })
                    .collect();

                ExportedSample {
                    locations,
                    values: sample.values.clone(),
                    labels,
                }
            })
            .collect::<Vec<_>>();
        samples.sort();
        samples
    }

    #[test]
    fn constructor_interns_well_known_strings() {
        let profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        assert_eq!(
            profile
                .dictionary
                .segment
                .get_string(DynamicStringIndex::EMPTY),
            Some("")
        );
        assert_eq!(
            profile
                .dictionary
                .segment
                .get_string(profile.well_known.local_root_span_id),
            Some("local root span id")
        );
        assert_eq!(
            profile
                .dictionary
                .segment
                .get_string(profile.well_known.trace_endpoint),
            Some("trace endpoint")
        );
        assert_eq!(
            profile
                .dictionary
                .segment
                .get_string(profile.well_known.end_timestamp_ns),
            Some("end_timestamp_ns")
        );
    }

    #[test]
    fn function_indices_follow_insertion_order_and_dedup() {
        let profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
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
            stored.packed_location_ids.as_ptr() as usize % align_of::<u64>(),
            0
        );
        assert_eq!(stored.location_ids_len(), 4);
        assert_eq!(stored.packed_location_ids.len(), 2);
        let mut unpacked_location_ids = Vec::new();
        stored.extend_location_ids_u64(&mut unpacked_location_ids);
        let resolved_locations = unpacked_location_ids
            .iter()
            .map(|id| {
                profile
                    .period_local
                    .locations
                    .entries
                    .get_index(*id as usize)
                    .copied()
                    .expect("stored location")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            resolved_locations,
            vec![
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
    fn clear_all_data_preserves_dictionary_handles() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        profile.clear_all_data().expect("clear all");
        assert_eq!(profile.intern_string("func").expect("same name"), name);
        assert_eq!(
            profile.intern_function(name, file).expect("same func"),
            func
        );
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

        assert_eq!(exported_samples(&profile_a), exported_samples(&profile_b));
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
    fn mixed_round_trip_preserves_sample_semantics() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let thread_id_key = profile.intern_string("thread id").expect("thread id");
        let thread_name_key = profile.intern_string("thread name").expect("thread name");

        let file = profile.intern_string("file.py").expect("file");
        let leaf_name = profile.intern_string("leaf").expect("leaf");
        let root_name = profile.intern_string("root").expect("root");
        let leaf = profile.intern_function(leaf_name, file).expect("leaf");
        let root = profile.intern_function(root_name, file).expect("root");

        let stack_one = [DynamicLocation {
            function: leaf,
            line: 10,
        }];
        let stack_two = [
            DynamicLocation {
                function: leaf,
                line: 20,
            },
            DynamicLocation {
                function: root,
                line: 30,
            },
        ];

        profile.set_endpoint(77, "/users/:id").expect("endpoint");

        let aggregated_labels = [
            DynamicLabel {
                key: thread_id_key,
                str: "",
                num: 7,
            },
            DynamicLabel {
                key: thread_name_key,
                str: "worker-a",
                num: 0,
            },
        ];
        let endpoint_labels = [
            DynamicLabel {
                key: profile.well_known.local_root_span_id,
                str: "",
                num: 77,
            },
            DynamicLabel {
                key: thread_name_key,
                str: "worker-b",
                num: 0,
            },
        ];

        profile
            .add_sample_by_locations(
                &stack_one,
                DynamicSample {
                    values: &[3, 1],
                    labels: &aggregated_labels,
                },
                0,
            )
            .expect("aggregated first");
        profile
            .add_sample_by_locations(
                &stack_one,
                DynamicSample {
                    values: &[3, 1],
                    labels: &aggregated_labels,
                },
                0,
            )
            .expect("aggregated second");
        profile
            .add_sample_by_locations(
                &stack_one,
                DynamicSample {
                    values: &[5, 1],
                    labels: &aggregated_labels,
                },
                123_456,
            )
            .expect("timestamped");
        profile
            .add_sample_by_locations(
                &stack_two,
                DynamicSample {
                    values: &[9, 3],
                    labels: &endpoint_labels,
                },
                0,
            )
            .expect("endpoint sample");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );

        assert_eq!(
            exported_samples(&profile),
            vec![
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "leaf".to_string(),
                        line: 10,
                    }],
                    values: vec![5, 1],
                    labels: vec![
                        ExportedLabel {
                            key: "end_timestamp_ns".to_string(),
                            str_value: None,
                            num: 123_456,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread id".to_string(),
                            str_value: None,
                            num: 7,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("worker-a".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "leaf".to_string(),
                        line: 10,
                    }],
                    values: vec![6, 2],
                    labels: vec![
                        ExportedLabel {
                            key: "thread id".to_string(),
                            str_value: None,
                            num: 7,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("worker-a".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
                ExportedSample {
                    locations: vec![
                        ExportedFrame {
                            function: "leaf".to_string(),
                            line: 20,
                        },
                        ExportedFrame {
                            function: "root".to_string(),
                            line: 30,
                        },
                    ],
                    values: vec![9, 3],
                    labels: vec![
                        ExportedLabel {
                            key: "local root span id".to_string(),
                            str_value: None,
                            num: 77,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("worker-b".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "trace endpoint".to_string(),
                            str_value: Some("/users/:id".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn serializing_clears_period_local_samples_between_exports() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.py").expect("file");
        let first_name = profile.intern_string("first").expect("first");
        let second_name = profile.intern_string("second").expect("second");
        let first = profile.intern_function(first_name, file).expect("first");
        let second = profile.intern_function(second_name, file).expect("second");
        let thread_name_key = profile.intern_string("thread name").expect("thread name");

        profile
            .add_sample_by_locations(
                &[DynamicLocation {
                    function: first,
                    line: 11,
                }],
                DynamicSample {
                    values: &[2, 1],
                    labels: &[DynamicLabel {
                        key: thread_name_key,
                        str: "before-clear",
                        num: 0,
                    }],
                },
                0,
            )
            .expect("first sample");

        let first_export = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        assert_eq!(
            exported_samples(&first_export),
            vec![ExportedSample {
                locations: vec![ExportedFrame {
                    function: "first".to_string(),
                    line: 11,
                }],
                values: vec![2, 1],
                labels: vec![ExportedLabel {
                    key: "thread name".to_string(),
                    str_value: Some("before-clear".to_string()),
                    num: 0,
                    num_unit: None,
                }],
            }]
        );

        profile
            .add_sample_by_locations(
                &[DynamicLocation {
                    function: second,
                    line: 22,
                }],
                DynamicSample {
                    values: &[7, 4],
                    labels: &[DynamicLabel {
                        key: thread_name_key,
                        str: "after-clear",
                        num: 0,
                    }],
                },
                0,
            )
            .expect("second sample");

        let second_export = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );
        assert_eq!(
            exported_samples(&second_export),
            vec![ExportedSample {
                locations: vec![ExportedFrame {
                    function: "second".to_string(),
                    line: 22,
                }],
                values: vec![7, 4],
                labels: vec![ExportedLabel {
                    key: "thread name".to_string(),
                    str_value: Some("after-clear".to_string()),
                    num: 0,
                    num_unit: None,
                }],
            }]
        );
    }

    #[test]
    fn dynamic_and_native_round_trip_match_for_endpoint_and_upscaling() {
        let sample_types = sample_types();
        let mut dynamic = DynamicProfile::try_new(&sample_types, None, None).expect("dynamic");
        let mut native = crate::internal::Profile::new(&sample_types, None);

        let kind_key = dynamic.intern_string("kind").expect("kind");
        let thread_name_key = dynamic.intern_string("thread name").expect("thread name");

        let file = dynamic.intern_string("file.py").expect("file");
        let leaf_name = dynamic.intern_string("leaf").expect("leaf");
        let root_name = dynamic.intern_string("root").expect("root");
        let leaf = dynamic.intern_function(leaf_name, file).expect("leaf");
        let root = dynamic.intern_function(root_name, file).expect("root");

        let dynamic_leaf_stack = [DynamicLocation {
            function: leaf,
            line: 10,
        }];
        let dynamic_root_stack = [
            DynamicLocation {
                function: leaf,
                line: 20,
            },
            DynamicLocation {
                function: root,
                line: 30,
            },
        ];

        let mapping = api::Mapping::default();
        let native_leaf_stack = vec![api::Location {
            mapping,
            function: api::Function {
                name: "leaf",
                system_name: "",
                filename: "file.py",
            },
            address: 0,
            line: 10,
        }];
        let native_root_stack = vec![
            api::Location {
                mapping,
                function: api::Function {
                    name: "leaf",
                    system_name: "",
                    filename: "file.py",
                },
                address: 0,
                line: 20,
            },
            api::Location {
                mapping,
                function: api::Function {
                    name: "root",
                    system_name: "",
                    filename: "file.py",
                },
                address: 0,
                line: 30,
            },
        ];

        dynamic
            .set_endpoint(11, "/users/:id")
            .expect("dynamic endpoint");
        native
            .add_endpoint(11, Cow::Borrowed("/users/:id"))
            .expect("native endpoint");

        dynamic
            .add_upscaling_rule_proportional(&[0], kind_key, "alloc", 2.0)
            .expect("dynamic scale value 0");
        dynamic
            .add_upscaling_rule_proportional(&[1], kind_key, "alloc", 3.0)
            .expect("dynamic scale value 1");
        native
            .add_upscaling_rule(
                &[0],
                "kind",
                "alloc",
                api::UpscalingInfo::Proportional { scale: 2.0 },
            )
            .expect("native scale value 0");
        native
            .add_upscaling_rule(
                &[1],
                "kind",
                "alloc",
                api::UpscalingInfo::Proportional { scale: 3.0 },
            )
            .expect("native scale value 1");

        dynamic
            .add_sample_by_locations(
                &dynamic_leaf_stack,
                DynamicSample {
                    values: &[7, 11],
                    labels: &[
                        DynamicLabel {
                            key: kind_key,
                            str: "alloc",
                            num: 0,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: "worker-a",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .expect("dynamic alloc sample");
        native
            .try_add_sample(
                api::Sample {
                    locations: native_leaf_stack.clone(),
                    values: &[7, 11],
                    labels: vec![
                        api::Label {
                            key: "kind",
                            str: "alloc",
                            num: 0,
                            num_unit: "",
                        },
                        api::Label {
                            key: "thread name",
                            str: "worker-a",
                            num: 0,
                            num_unit: "",
                        },
                    ],
                },
                None,
            )
            .expect("native alloc sample");

        dynamic
            .add_sample_by_locations(
                &dynamic_root_stack,
                DynamicSample {
                    values: &[5, 1],
                    labels: &[
                        DynamicLabel {
                            key: dynamic.well_known.local_root_span_id,
                            str: "",
                            num: 11,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: "worker-b",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .expect("dynamic endpoint sample");
        native
            .try_add_sample(
                api::Sample {
                    locations: native_root_stack.clone(),
                    values: &[5, 1],
                    labels: vec![
                        api::Label {
                            key: "local root span id",
                            str: "",
                            num: 11,
                            num_unit: "",
                        },
                        api::Label {
                            key: "thread name",
                            str: "worker-b",
                            num: 0,
                            num_unit: "",
                        },
                    ],
                },
                None,
            )
            .expect("native endpoint sample");

        dynamic
            .add_sample_by_locations(
                &dynamic_leaf_stack,
                DynamicSample {
                    values: &[9, 2],
                    labels: &[DynamicLabel {
                        key: kind_key,
                        str: "wall",
                        num: 0,
                    }],
                },
                0,
            )
            .expect("dynamic unmatched sample");
        native
            .try_add_sample(
                api::Sample {
                    locations: native_leaf_stack,
                    values: &[9, 2],
                    labels: vec![api::Label {
                        key: "kind",
                        str: "wall",
                        num: 0,
                        num_unit: "",
                    }],
                },
                None,
            )
            .expect("native unmatched sample");

        let dynamic_profile = decode_profile(
            &dynamic
                .serialize_and_clear_period_local_data(None, None)
                .expect("dynamic serialize")
                .buffer,
        );
        let native_profile = roundtrip_to_pprof(native).expect("native round trip");

        assert_eq!(
            exported_samples(&dynamic_profile),
            exported_samples(&native_profile)
        );
    }

    #[test]
    fn duplicate_label_keys_are_rejected_in_debug_builds() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");
        let key = profile.intern_string("thread name").expect("thread name");

        let error = profile
            .add_sample_by_locations(
                &[DynamicLocation {
                    function: func,
                    line: 42,
                }],
                DynamicSample {
                    values: &[1, 1],
                    labels: &[
                        DynamicLabel {
                            key,
                            str: "worker-a",
                            num: 0,
                        },
                        DynamicLabel {
                            key,
                            str: "worker-b",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .unwrap_err();

        assert!(matches!(error, DynamicProfileError::DuplicateLabelKey));
    }

    #[test]
    fn reserved_labels_are_rejected_in_debug_builds() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let name = profile.intern_string("func").expect("name");
        let file = profile.intern_string("file.py").expect("file");
        let func = profile.intern_function(name, file).expect("func");

        let root_span_error = profile
            .add_sample_by_locations(
                &[DynamicLocation {
                    function: func,
                    line: 42,
                }],
                DynamicSample {
                    values: &[1, 1],
                    labels: &[DynamicLabel {
                        key: profile.well_known.local_root_span_id,
                        str: "not-a-number",
                        num: 0,
                    }],
                },
                0,
            )
            .unwrap_err();
        assert!(matches!(
            root_span_error,
            DynamicProfileError::InvalidLabelValue
        ));

        let timestamp_error = profile
            .add_sample_by_locations(
                &[DynamicLocation {
                    function: func,
                    line: 42,
                }],
                DynamicSample {
                    values: &[1, 1],
                    labels: &[DynamicLabel {
                        key: profile.well_known.end_timestamp_ns,
                        str: "",
                        num: 123,
                    }],
                },
                0,
            )
            .unwrap_err();
        assert!(matches!(
            timestamp_error,
            DynamicProfileError::InvalidLabelValue
        ));
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

    #[test]
    fn endpoint_enrichment_applies_per_root_span_and_skips_unmapped_samples() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.php").expect("file");
        let leaf_name = profile.intern_string("leaf").expect("leaf");
        let root_name = profile.intern_string("root").expect("root");
        let leaf = profile.intern_function(leaf_name, file).expect("leaf");
        let root = profile.intern_function(root_name, file).expect("root");
        let thread_name_key = profile.intern_string("thread name").expect("thread name");

        let leaf_stack = [DynamicLocation {
            function: leaf,
            line: 10,
        }];
        let root_stack = [
            DynamicLocation {
                function: leaf,
                line: 20,
            },
            DynamicLocation {
                function: root,
                line: 30,
            },
        ];

        profile.set_endpoint(11, "/users/:id").expect("endpoint 11");
        profile.set_endpoint(22, "/posts/:id").expect("endpoint 22");

        profile
            .add_sample_by_locations(
                &leaf_stack,
                DynamicSample {
                    values: &[1, 1],
                    labels: &[
                        DynamicLabel {
                            key: profile.well_known.local_root_span_id,
                            str: "",
                            num: 11,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: "mapped-a",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .expect("mapped sample a");
        profile
            .add_sample_by_locations(
                &root_stack,
                DynamicSample {
                    values: &[2, 1],
                    labels: &[
                        DynamicLabel {
                            key: profile.well_known.local_root_span_id,
                            str: "",
                            num: 22,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: "mapped-b",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .expect("mapped sample b");
        profile
            .add_sample_by_locations(
                &leaf_stack,
                DynamicSample {
                    values: &[3, 1],
                    labels: &[
                        DynamicLabel {
                            key: profile.well_known.local_root_span_id,
                            str: "",
                            num: 33,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: "unmapped",
                            num: 0,
                        },
                    ],
                },
                0,
            )
            .expect("unmapped sample");
        profile
            .add_sample_by_locations(
                &leaf_stack,
                DynamicSample {
                    values: &[4, 1],
                    labels: &[DynamicLabel {
                        key: thread_name_key,
                        str: "no-root-span",
                        num: 0,
                    }],
                },
                0,
            )
            .expect("sample without root span");

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );

        assert_eq!(
            exported_samples(&profile),
            vec![
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "leaf".to_string(),
                        line: 10,
                    }],
                    values: vec![1, 1],
                    labels: vec![
                        ExportedLabel {
                            key: "local root span id".to_string(),
                            str_value: None,
                            num: 11,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("mapped-a".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "trace endpoint".to_string(),
                            str_value: Some("/users/:id".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "leaf".to_string(),
                        line: 10,
                    }],
                    values: vec![3, 1],
                    labels: vec![
                        ExportedLabel {
                            key: "local root span id".to_string(),
                            str_value: None,
                            num: 33,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("unmapped".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "leaf".to_string(),
                        line: 10,
                    }],
                    values: vec![4, 1],
                    labels: vec![ExportedLabel {
                        key: "thread name".to_string(),
                        str_value: Some("no-root-span".to_string()),
                        num: 0,
                        num_unit: None,
                    }],
                },
                ExportedSample {
                    locations: vec![
                        ExportedFrame {
                            function: "leaf".to_string(),
                            line: 20,
                        },
                        ExportedFrame {
                            function: "root".to_string(),
                            line: 30,
                        },
                    ],
                    values: vec![2, 1],
                    labels: vec![
                        ExportedLabel {
                            key: "local root span id".to_string(),
                            str_value: None,
                            num: 22,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "thread name".to_string(),
                            str_value: Some("mapped-b".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                        ExportedLabel {
                            key: "trace endpoint".to_string(),
                            str_value: Some("/posts/:id".to_string()),
                            num: 0,
                            num_unit: None,
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn multiple_upscaling_rules_round_trip_with_selective_matches() {
        let mut profile = DynamicProfile::try_new(&sample_types(), None, None).expect("profile");
        let file = profile.intern_string("file.rb").expect("file");
        let func_name = profile.intern_string("func").expect("func");
        let func = profile.intern_function(func_name, file).expect("func");
        let kind_key = profile.intern_string("kind").expect("kind");
        let location = [DynamicLocation {
            function: func,
            line: 17,
        }];

        profile
            .add_upscaling_rule_proportional(&[0], kind_key, "alloc", 2.0)
            .expect("alloc scale value 0");
        profile
            .add_upscaling_rule_proportional(&[1], kind_key, "alloc", 3.0)
            .expect("alloc scale value 1");
        profile
            .add_upscaling_rule_proportional(&[0], kind_key, "cpu", 5.0)
            .expect("cpu scale value 0");

        for (kind, values) in [("alloc", [7, 11]), ("cpu", [7, 11]), ("wall", [7, 11])] {
            profile
                .add_sample_by_locations(
                    &location,
                    DynamicSample {
                        values: &values,
                        labels: &[DynamicLabel {
                            key: kind_key,
                            str: kind,
                            num: 0,
                        }],
                    },
                    0,
                )
                .expect("sample");
        }

        let profile = decode_profile(
            &profile
                .serialize_and_clear_period_local_data(None, None)
                .expect("serialize")
                .buffer,
        );

        assert_eq!(
            exported_samples(&profile),
            vec![
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "func".to_string(),
                        line: 17,
                    }],
                    values: vec![7, 11],
                    labels: vec![ExportedLabel {
                        key: "kind".to_string(),
                        str_value: Some("wall".to_string()),
                        num: 0,
                        num_unit: None,
                    }],
                },
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "func".to_string(),
                        line: 17,
                    }],
                    values: vec![14, 33],
                    labels: vec![ExportedLabel {
                        key: "kind".to_string(),
                        str_value: Some("alloc".to_string()),
                        num: 0,
                        num_unit: None,
                    }],
                },
                ExportedSample {
                    locations: vec![ExportedFrame {
                        function: "func".to_string(),
                        line: 17,
                    }],
                    values: vec![35, 11],
                    labels: vec![ExportedLabel {
                        key: "kind".to_string(),
                        str_value: Some("cpu".to_string()),
                        num: 0,
                        num_unit: None,
                    }],
                },
            ]
        );
    }
}
