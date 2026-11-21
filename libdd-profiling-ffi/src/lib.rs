// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C API for `libdd-profiling` v2. See `include/datadog/profiling.h`.
//!
//! - Tables (`ddog_prof_StringTable`, `ddog_prof_FunctionTable`) are counted handles: `_clone` adds
//!   a handle, `_drop` removes one.
//! - A `ddog_prof_Profile` is not locked; callers serialize access to it.
//! - `ddog_prof_Profile_datadog_request` and `_otlp_request` lower a profile to pprof or OTLP as a
//!   ready-to-send HTTP request; the caller's own HTTP client sends it. Either, or both, may run on
//!   the same profile.
//! - Every function returns a [`Status`]; 0 is success. Nothing panics or aborts: release artifacts
//!   are checked for that.

#![no_std]

#[cfg(ddprof_ffi_cdylib)]
mod artifact;

use core::alloc::Layout;
use core::ffi::{CStr, c_char, c_void};
use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use libdd_profiling::collections::alloc::{Allocator, Global};
use libdd_profiling::collections::vec::TryVec;
use libdd_profiling::encode::EncodeError;
#[cfg(any(feature = "pprof", feature = "otel"))]
use libdd_profiling::encode::Period as CorePeriod;
#[cfg(feature = "otel")]
use libdd_profiling::encode::otlp::Scope as CoreScope;
#[cfg(feature = "otel")]
use libdd_profiling::encode::otlp::{OtlpSampleType, ProfileSpec};
#[cfg(any(feature = "pprof", feature = "otel"))]
use libdd_profiling::encode::{EncodeOptions as CoreEncodeOptions, Resource as CoreResource};
use libdd_profiling::intern_table::{
    Config, FunctionId, FunctionTable, InitError, InternError, InternTable, Record, StringId,
    StringTable, TooManyHandles,
};
#[cfg(feature = "otel")]
use libdd_profiling::profile::SampleTypeId;
use libdd_profiling::profile::{
    Frame, Function, Label as CoreLabel, LabelValue, Link, Profile as CoreProfile, ProfileError,
    ProfileStacks, Sample as CoreSample, SampleType as CoreSampleType, SampleValue,
};
use libdd_profiling::request;
#[cfg(any(feature = "pprof", feature = "otel"))]
use libdd_profiling::request::Compression;
#[cfg(feature = "pprof")]
use libdd_profiling::request::DatadogOptions as CoreDatadogOptions;

/// The outcome of a call. `Ok` is 0.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Ok = 0,
    InvalidArgument,
    InvalidUtf8,
    OutOfMemory,
    Busy,
    Full,
    ArenaFull,
    TooLong,
    LockFailed,
    Corrupt,
    MappingFailed,
    UnsupportedPageSize,
    TooLarge,
    NothingToEncode,
    TooMany,
    TooManyHandles,
    Compression,
    CompressionUnavailable,
}

impl From<InitError> for Status {
    fn from(error: InitError) -> Self {
        match error {
            InitError::InvalidConfig => Self::InvalidArgument,
            InitError::UnsupportedPageSize => Self::UnsupportedPageSize,
            InitError::MappingFailed => Self::MappingFailed,
            InitError::OutOfMemory => Self::OutOfMemory,
            InitError::LockFailed => Self::LockFailed,
        }
    }
}

impl From<InternError> for Status {
    fn from(error: InternError) -> Self {
        match error {
            InternError::Busy => Self::Busy,
            InternError::Full => Self::Full,
            InternError::ArenaFull => Self::ArenaFull,
            InternError::TooLong => Self::TooLong,
            InternError::OutOfMemory => Self::OutOfMemory,
            InternError::LockFailed => Self::LockFailed,
            InternError::Corrupt => Self::Corrupt,
        }
    }
}

impl From<ProfileError> for Status {
    fn from(error: ProfileError) -> Self {
        match error {
            ProfileError::OutOfMemory => Self::OutOfMemory,
            ProfileError::InvalidArgument => Self::InvalidArgument,
            ProfileError::TooMany => Self::TooMany,
        }
    }
}

impl From<EncodeError> for Status {
    fn from(error: EncodeError) -> Self {
        match error {
            EncodeError::OutOfMemory => Self::OutOfMemory,
            EncodeError::TooLarge => Self::TooLarge,
            EncodeError::NothingToEncode => Self::NothingToEncode,
            EncodeError::InvalidArgument => Self::InvalidArgument,
            EncodeError::Compression => Self::Compression,
            EncodeError::CompressionUnavailable => Self::CompressionUnavailable,
        }
    }
}

impl From<TooManyHandles> for Status {
    fn from(TooManyHandles: TooManyHandles) -> Self {
        Self::TooManyHandles
    }
}

/// A static, NUL-terminated description of `status`.
#[unsafe(no_mangle)]
pub const extern "C" fn ddog_prof_Status_message(status: Status) -> *const c_char {
    let message: &CStr = match status {
        Status::Ok => c"ok",
        Status::InvalidArgument => c"invalid argument",
        Status::InvalidUtf8 => c"string is not valid UTF-8",
        Status::OutOfMemory => c"out of memory",
        Status::Busy => c"table is busy; retry later",
        Status::Full => c"table is full",
        Status::ArenaFull => c"string table arena is full",
        Status::TooLong => c"string is too long",
        Status::LockFailed => c"table process lock failed",
        Status::Corrupt => c"table memory is inconsistent",
        Status::MappingFailed => c"could not reserve memory",
        Status::UnsupportedPageSize => c"unsupported OS page size",
        Status::TooLarge => c"encoded profile exceeds the size limit",
        Status::NothingToEncode => c"no sample type for this format",
        Status::TooMany => c"too many distinct stacks, labels or links",
        Status::TooManyHandles => c"too many table handles",
        Status::Compression => c"compression failed",
        Status::CompressionUnavailable => c"compression not built in",
    };
    message.as_ptr()
}

/// A UTF-8 string slice; need not be NUL-terminated. `ptr` may be null when
/// `len` is 0.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

/// A byte slice. `ptr` may be null when `len` is 0.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Bytes {
    pub ptr: *const u8,
    pub len: usize,
}

impl Str {
    const fn of(s: &str) -> Self {
        Self {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// A slice from a C pointer and length.
///
/// # Safety
///
/// Unless `len` is 0, `ptr` points to `len` valid `T`s that outlive `'a`.
unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> Result<&'a [T], Status> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() || !ptr.is_aligned() {
        return Err(Status::InvalidArgument);
    }
    // SAFETY: the caller's contract; non-null and aligned checked above.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

/// A mutable slice from a C pointer and length.
///
/// # Safety
///
/// Unless `len` is 0, `ptr` points to `len` valid `T`s, used by nothing else
/// until `'a` ends.
unsafe fn slice_mut<'a, T>(ptr: *mut T, len: usize) -> Result<&'a mut [T], Status> {
    if len == 0 {
        return Ok(&mut []);
    }
    if ptr.is_null() || !ptr.is_aligned() {
        return Err(Status::InvalidArgument);
    }
    // SAFETY: the caller's contract; non-null and aligned checked above.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
}

/// The `&str` of a [`Str`], checked to be UTF-8.
///
/// # Safety
///
/// As for [`slice`].
unsafe fn text<'a>(s: Str) -> Result<&'a str, Status> {
    // SAFETY: forwarded.
    let bytes = unsafe { slice(s.ptr, s.len)? };
    core::str::from_utf8(bytes).map_err(|_| Status::InvalidUtf8)
}

/// Writes `value` to a non-null `out`.
///
/// # Safety
///
/// `out` is null or valid for a write of `T`.
unsafe fn put<T>(out: *mut T, value: T) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: non-null, and valid by the caller's contract.
    unsafe { out.write(value) };
    Status::Ok
}

const fn status(result: Result<(), Status>) -> Status {
    match result {
        Ok(()) => Status::Ok,
        Err(status) => status,
    }
}

/// `value`, moved to the heap without aborting on allocation failure.
fn try_box<T>(value: T) -> Result<NonNull<T>, Status> {
    let block = Global
        .allocate(Layout::new::<T>())
        .map_err(|_| Status::OutOfMemory)?;
    let ptr = block.cast::<T>();
    // SAFETY: freshly allocated for one `T`.
    unsafe { ptr.write(value) };
    Ok(ptr)
}

/// Drops and frees a value from [`try_box`].
///
/// # Safety
///
/// `ptr` came from `try_box::<T>` and is not used again.
unsafe fn drop_box<T>(ptr: NonNull<T>) {
    // SAFETY: the caller hands over the box.
    unsafe {
        ptr::drop_in_place(ptr.as_ptr());
        Global.deallocate(ptr.cast(), Layout::new::<T>());
    }
}

/// Borrows a table handle that C still owns.
///
/// # Safety
///
/// `raw` is null or came from `into_raw` on an `InternTable<R>` that C has
/// not dropped.
unsafe fn borrow_table<R: Record>(
    raw: *mut c_void,
) -> Result<ManuallyDrop<InternTable<R>>, Status> {
    let raw = NonNull::new(raw).ok_or(Status::InvalidArgument)?;
    // SAFETY: the caller's contract; `ManuallyDrop` keeps C's count.
    Ok(ManuallyDrop::new(unsafe { InternTable::from_raw(raw) }))
}

fn lock_dir<'a>(dir: *const c_char) -> Option<&'a CStr> {
    // SAFETY: a non-null `dir` is a NUL-terminated string (the header's
    // contract) that outlives the call.
    (!dir.is_null()).then(|| unsafe { CStr::from_ptr(dir) })
}

// ---- Tables -------------------------------------------------------------

/// Creates a string table. `lock_dir` (required on Unix, NULL on Windows) is
/// where its process lock file is created and unlinked.
///
/// # Safety
///
/// `lock_dir` is NULL or NUL-terminated; `out` is valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_new(
    regions: u8,
    arena_bytes: usize,
    lock_dir_path: *const c_char,
    out: *mut *mut c_void,
) -> Status {
    let config = Config {
        regions,
        arena_bytes,
    };
    match StringTable::new(config, lock_dir(lock_dir_path)) {
        // SAFETY: the caller's contract.
        Ok(table) => unsafe { put(out, table.into_raw().as_ptr()) },
        Err(error) => error.into(),
    }
}

/// Creates a string table that also holds `reserved`, such as placeholders
/// for when interning fails later: `ids[i]` gets `reserved[i]`'s id.
///
/// The two lengths must be equal. On error the table is not created and
/// `ids` is all zeros (when it is valid).
///
/// # Safety
///
/// As for [`ddog_prof_StringTable_new`]; `reserved` points to `reserved_len`
/// strings and `ids` to `ids_len` ids (either may be NULL when its length is
/// 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_new_with_reserved(
    regions: u8,
    arena_bytes: usize,
    lock_dir_path: *const c_char,
    reserved: *const Str,
    reserved_len: usize,
    ids: *mut u32,
    ids_len: usize,
    out: *mut *mut c_void,
) -> Status {
    let config = Config {
        regions,
        arena_bytes,
    };
    let result = (|| {
        // SAFETY: the caller's contract; `StringId` is a transparent `u32`.
        let ids = unsafe { slice_mut(ids.cast::<StringId>(), ids_len)? };
        for id in ids.iter_mut() {
            *id = StringId::ZERO;
        }
        if reserved_len != ids_len {
            return Err(Status::InvalidArgument);
        }
        // SAFETY: the caller's contract.
        let reserved = unsafe { slice(reserved, reserved_len)? };
        let mut strings =
            TryVec::try_with_capacity_in(reserved_len, Global).map_err(|_| Status::OutOfMemory)?;
        for &s in reserved {
            // SAFETY: the caller's contract.
            let _ = strings.push_within_capacity(unsafe { text(s)? });
        }
        Ok(StringTable::with_reserved(
            config,
            lock_dir(lock_dir_path),
            &strings,
            ids,
        )?)
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(table) => unsafe { put(out, table.into_raw().as_ptr()) },
        Err(status) => status,
    }
}

/// Creates a function table. See [`ddog_prof_StringTable_new`].
///
/// # Safety
///
/// As for [`ddog_prof_StringTable_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_new(
    regions: u8,
    lock_dir_path: *const c_char,
    out: *mut *mut c_void,
) -> Status {
    let config = Config {
        regions,
        arena_bytes: 0,
    };
    match FunctionTable::new(config, lock_dir(lock_dir_path)) {
        // SAFETY: the caller's contract.
        Ok(table) => unsafe { put(out, table.into_raw().as_ptr()) },
        Err(error) => error.into(),
    }
}

/// Creates a function table that also holds `reserved` (functions whose
/// fields are string ids): `ids[i]` gets `reserved[i]`'s id. See
/// [`ddog_prof_StringTable_new_with_reserved`].
///
/// # Safety
///
/// As for [`ddog_prof_StringTable_new_with_reserved`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_new_with_reserved(
    regions: u8,
    lock_dir_path: *const c_char,
    reserved: *const Function,
    reserved_len: usize,
    ids: *mut u32,
    ids_len: usize,
    out: *mut *mut c_void,
) -> Status {
    let config = Config {
        regions,
        arena_bytes: 0,
    };
    let result = (|| {
        // SAFETY: the caller's contract; `FunctionId` is a transparent `u32`.
        let ids = unsafe { slice_mut(ids.cast::<FunctionId>(), ids_len)? };
        for id in ids.iter_mut() {
            *id = FunctionId::ZERO;
        }
        if reserved_len != ids_len {
            return Err(Status::InvalidArgument);
        }
        // SAFETY: the caller's contract.
        let reserved = unsafe { slice(reserved, reserved_len)? };
        Ok(FunctionTable::with_reserved(
            config,
            lock_dir(lock_dir_path),
            reserved,
            ids,
        )?)
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(table) => unsafe { put(out, table.into_raw().as_ptr()) },
        Err(status) => status,
    }
}

/// # Safety
///
/// `table` is a live handle from this library of the right kind.
unsafe fn clone_table<R: Record>(table: *mut c_void, out: *mut *mut c_void) -> Status {
    // SAFETY: forwarded.
    let result = unsafe { borrow_table::<R>(table) }
        .and_then(|table| table.try_clone().map_err(Status::from));
    match result {
        // SAFETY: the caller's contract.
        Ok(copy) => unsafe { put(out, copy.into_raw().as_ptr()) },
        Err(status) => status,
    }
}

/// # Safety
///
/// `table` is NULL or a live handle of the right kind, not used again.
unsafe fn drop_table<R: Record>(table: *mut c_void) {
    if let Some(raw) = NonNull::new(table) {
        // SAFETY: the caller hands over the handle.
        drop(unsafe { InternTable::<R>::from_raw(raw) });
    }
}

/// Another handle to the same string table.
///
/// # Safety
///
/// `table` is a live string table handle; `out` is valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_clone(
    table: *mut c_void,
    out: *mut *mut c_void,
) -> Status {
    // SAFETY: forwarded.
    unsafe { clone_table::<libdd_profiling::intern_table::Str>(table, out) }
}

/// Another handle to the same function table.
///
/// # Safety
///
/// `table` is a live function table handle; `out` is valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_clone(
    table: *mut c_void,
    out: *mut *mut c_void,
) -> Status {
    // SAFETY: forwarded.
    unsafe { clone_table::<Function>(table, out) }
}

/// Drops a string table handle; the last one in a process unmaps it there.
///
/// # Safety
///
/// `table` is NULL or a live string table handle, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_drop(table: *mut c_void) {
    // SAFETY: forwarded.
    unsafe { drop_table::<libdd_profiling::intern_table::Str>(table) }
}

/// Drops a function table handle.
///
/// # Safety
///
/// `table` is NULL or a live function table handle, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_drop(table: *mut c_void) {
    // SAFETY: forwarded.
    unsafe { drop_table::<Function>(table) }
}

/// # Safety
///
/// `table` is NULL or a live handle of the right kind.
unsafe fn fork_table<R: Record>(table: *mut c_void, prepare: bool) -> Status {
    // SAFETY: forwarded.
    let Ok(table) = (unsafe { borrow_table::<R>(table) }) else {
        return Status::InvalidArgument;
    };
    if prepare {
        table.prepare_fork_unguarded();
    } else {
        // SAFETY: the header's contract: after a matching prepare.
        unsafe { table.finish_fork() };
    }
    Status::Ok
}

/// Takes the string table's thread lock before `fork`, waiting for it.
///
/// A child then cannot inherit the lock held by another thread. Call
/// `ddog_prof_StringTable_finish_fork` once in the parent and once in the
/// child afterwards. The child keeps using the same (shared) table.
///
/// # Safety
///
/// `table` is a live string table handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_prepare_fork(table: *mut c_void) -> Status {
    // SAFETY: forwarded.
    unsafe { fork_table::<libdd_profiling::intern_table::Str>(table, true) }
}

/// Releases the lock taken by `ddog_prof_StringTable_prepare_fork`.
///
/// # Safety
///
/// After a matching prepare, once per process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_finish_fork(table: *mut c_void) -> Status {
    // SAFETY: forwarded.
    unsafe { fork_table::<libdd_profiling::intern_table::Str>(table, false) }
}

/// The function table's `prepare_fork`; see the string table's.
///
/// # Safety
///
/// `table` is a live function table handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_prepare_fork(table: *mut c_void) -> Status {
    // SAFETY: forwarded.
    unsafe { fork_table::<Function>(table, true) }
}

/// The function table's `finish_fork`; see the string table's.
///
/// # Safety
///
/// After a matching prepare, once per process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_finish_fork(table: *mut c_void) -> Status {
    // SAFETY: forwarded.
    unsafe { fork_table::<Function>(table, false) }
}

/// Interns a string; `*out` gets its id.
///
/// # Safety
///
/// `table` is a live string table handle; `s` is valid; `out` is valid for
/// a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_StringTable_intern(
    table: *mut c_void,
    s: Str,
    out: *mut u32,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let table = unsafe { borrow_table::<libdd_profiling::intern_table::Str>(table)? };
        // SAFETY: the caller's contract.
        let s = unsafe { text(s)? };
        Ok(table.intern(s)?.index())
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(id) => unsafe { put(out, id) },
        Err(status) => status,
    }
}

/// Interns a function, whose fields are string ids; `*out` gets its id.
///
/// # Safety
///
/// `table` is a live function table handle; `function` and `out` are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_FunctionTable_intern(
    table: *mut c_void,
    function: *const Function,
    out: *mut u32,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let table = unsafe { borrow_table::<Function>(table)? };
        // SAFETY: a non-null `function` is valid (the caller's contract).
        let function = unsafe { function.as_ref() }.ok_or(Status::InvalidArgument)?;
        Ok(table.intern(*function)?.index())
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(id) => unsafe { put(out, id) },
        Err(status) => status,
    }
}

// ---- Profiles -----------------------------------------------------------

type InnerProfile = CoreProfile<StringTable, FunctionTable, ProfileStacks>;

/// A profile, with scratch space for converting labels.
pub struct Profile {
    profile: InnerProfile,
    labels: TryVec<CoreLabel, Global>,
}

/// A sample type: string ids of its name and unit. Its id is its position
/// in the types given to [`ddog_prof_Profile_new`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SampleType {
    pub name: u32,
    pub unit: u32,
}

/// A sampling period, as a format records it: what is counted between
/// samples and how much. It does not affect any value.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Period {
    pub name: Str,
    pub unit: Str,
    pub value: i64,
}

/// Reads an optional period.
///
/// # Safety
///
/// `period` is NULL or valid, with valid strings.
#[cfg(any(feature = "pprof", feature = "otel"))]
unsafe fn period<'a>(period: *const Period) -> Result<Option<CorePeriod<'a>>, Status> {
    // SAFETY: the caller's contract.
    let Some(p) = (unsafe { period.as_ref() }) else {
        return Ok(None);
    };
    // SAFETY: the caller's contract.
    unsafe {
        Ok(Some(CorePeriod {
            name: text(p.name)?,
            unit: text(p.unit)?,
            value: p.value,
        }))
    }
}

/// A label: a string, or a number with an optional unit.
///
/// `num_unit` is a string id (0 for none). A string label uses `str_value`
/// when it is non-empty: that string is kept by the profile until its next
/// reset, which suits high-cardinality values. Otherwise `str` is a string
/// table id.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Label {
    pub key: u32,
    pub str: u32,
    pub str_value: Str,
    pub num: i64,
    pub num_unit: u32,
    pub is_num: bool,
}

/// A sample. `values` holds the values it measured, each sample type at
/// most once; types left out count as 0. `frames` are leaf first. All-zero
/// ids mean no span; `timestamp_ns` 0 aggregates.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub frames: *const Frame,
    pub frames_len: usize,
    pub values: *const SampleValue,
    pub values_len: usize,
    pub labels: *const Label,
    pub labels_len: usize,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub local_root_span_id: u64,
    pub timestamp_ns: i64,
}

/// Creates a profile that reads `strings` and `functions` (each gets one
/// more handle, released when the profile is dropped).
///
/// # Safety
///
/// The tables are live handles; `types` points to `types_len` sample
/// types; `out` is valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_new(
    strings: *mut c_void,
    functions: *mut c_void,
    types: *const SampleType,
    types_len: usize,
    start_ns: i64,
    out: *mut *mut Profile,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let strings = unsafe { borrow_table::<libdd_profiling::intern_table::Str>(strings)? };
        // SAFETY: the caller's contract.
        let functions = unsafe { borrow_table::<Function>(functions)? };
        // SAFETY: the caller's contract.
        let types = unsafe { slice(types, types_len)? };
        let mut core_types =
            TryVec::try_with_capacity_in(types.len(), Global).map_err(|_| Status::OutOfMemory)?;
        for t in types {
            let _ = core_types.push_within_capacity(CoreSampleType {
                name: string_id(t.name),
                unit: string_id(t.unit),
            });
        }
        let profile = CoreProfile::try_new(
            strings.try_clone()?,
            functions.try_clone()?,
            ProfileStacks::new(),
            &core_types,
            start_ns,
        )?;
        try_box(Profile {
            profile,
            labels: TryVec::new_in(Global),
        })
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(profile) => unsafe { put(out, profile.as_ptr()) },
        Err(status) => status,
    }
}

/// A string id from C; unknown ids resolve to nothing.
const fn string_id(index: u32) -> StringId {
    StringId::from_index(index)
}

/// # Safety
///
/// `profile` is NULL or a live profile.
unsafe fn profile_mut<'a>(profile: *mut Profile) -> Result<&'a mut Profile, Status> {
    // SAFETY: the caller's contract; C serializes access to the profile.
    unsafe { profile.as_mut() }.ok_or(Status::InvalidArgument)
}

/// Drops a profile and its table handles.
///
/// # Safety
///
/// `profile` is NULL or a live profile, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_drop(profile: *mut Profile) {
    if let Some(profile) = NonNull::new(profile) {
        // SAFETY: the caller hands over the profile.
        unsafe { drop_box(profile) };
    }
}

/// Adds a sample.
///
/// # Safety
///
/// `profile` is a live profile not used concurrently; `sample` and the
/// arrays it points to are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_add(
    profile: *mut Profile,
    sample: *const Sample,
) -> Status {
    status((|| {
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_mut(profile)? };
        // SAFETY: a non-null `sample` is valid (the caller's contract).
        let sample = unsafe { sample.as_ref() }.ok_or(Status::InvalidArgument)?;
        // SAFETY: the caller's contract.
        let (frames, values, labels) = unsafe {
            (
                slice(sample.frames, sample.frames_len)?,
                slice(sample.values, sample.values_len)?,
                slice(sample.labels, sample.labels_len)?,
            )
        };
        profile.labels.clear();
        profile
            .labels
            .try_reserve(labels.len())
            .map_err(|_| Status::OutOfMemory)?;
        for label in labels {
            let value = if label.is_num {
                LabelValue::Num {
                    value: label.num,
                    unit: string_id(label.num_unit),
                }
            } else if label.str_value.len > 0 {
                // SAFETY: the caller's contract.
                let text = unsafe { text(label.str_value)? };
                LabelValue::Local(profile.profile.intern_local(text)?)
            } else {
                LabelValue::Str(string_id(label.str))
            };
            let _ = profile.labels.push_within_capacity(CoreLabel {
                key: string_id(label.key),
                value,
            });
        }
        let Profile { profile, labels } = profile;
        profile.add_sample(CoreSample {
            stack: frames,
            values,
            labels,
            link: Link {
                trace_id: sample.trace_id,
                span_id: sample.span_id,
            },
            local_root_span_id: sample.local_root_span_id,
            timestamp_ns: sample.timestamp_ns,
        })?;
        Ok(())
    })())
}

/// Records the trace endpoint (the root span's resource name) of a local root
/// span. The profile keeps the name until its next reset; it never goes into
/// the shared string table.
///
/// # Safety
///
/// `profile` is a live profile not used concurrently; `endpoint` is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_set_endpoint(
    profile: *mut Profile,
    local_root_span_id: u64,
    endpoint: Str,
) -> Status {
    status((|| {
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_mut(profile)? };
        // SAFETY: the caller's contract.
        let endpoint = unsafe { text(endpoint)? };
        Ok(profile.profile.set_endpoint(local_root_span_id, endpoint)?)
    })())
}

/// Adds `count` hits of a trace endpoint; kept like the endpoint names of
/// `ddog_prof_Profile_set_endpoint`.
///
/// # Safety
///
/// `profile` is a live profile not used concurrently; `endpoint` is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_add_endpoint_count(
    profile: *mut Profile,
    endpoint: Str,
    count: i64,
) -> Status {
    status((|| {
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_mut(profile)? };
        // SAFETY: the caller's contract.
        let endpoint = unsafe { text(endpoint)? };
        Ok(profile.profile.add_endpoint_count(endpoint, count)?)
    })())
}

/// Drops every sample and starts a new period at `start_ns`.
///
/// # Safety
///
/// `profile` is a live profile not used concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_reset(profile: *mut Profile, start_ns: i64) -> Status {
    status((|| {
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_mut(profile)? };
        Ok(profile.profile.reset(start_ns)?)
    })())
}

// ---- Requests -----------------------------------------------------------

/// A `key:value` pair.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Tag {
    pub key: Str,
    pub value: Str,
}

/// What produced the profile. Empty strings and a zero process id are
/// omitted.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Resource {
    pub service: Str,
    pub env: Str,
    pub version: Str,
    pub host_name: Str,
    pub runtime_id: Str,
    pub language: Str,
    pub runtime_name: Str,
    pub runtime_version: Str,
    /// OTLP only: `process.runtime.description`.
    pub runtime_description: Str,
    pub profiler_version: Str,
    pub container_id: Str,
    /// OTLP only: `datadog.container.cgroup_inode`; 0 for none.
    pub cgroup_inode: u64,
    pub process_id: u32,
    pub tags: *const Tag,
    pub tags_len: usize,
}

/// A named attachment for the Datadog intake, such as
/// `code-provenance.json`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Attachment {
    pub name: Str,
    pub bytes: Bytes,
}

/// Extra inputs of a Datadog intake request. Empty strings are omitted.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DatadogOptions {
    pub library_name: Str,
    pub library_version: Str,
    pub family: Str,
    /// A JSON object for `event.json`'s `internal`.
    pub internal_json: Str,
    /// A JSON object for `event.json`'s `info`.
    pub info_json: Str,
    pub process_tags: Str,
    pub attachments: *const Attachment,
    pub attachments_len: usize,
    /// The pprof's sampling period; NULL for none.
    pub period: *const Period,
}

/// The OTLP instrumentation scope.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub name: Str,
    pub version: Str,
    pub attributes: *const Tag,
    pub attributes_len: usize,
}

/// An HTTP request to send: see `ddog_prof_Request_*`.
pub struct Request(request::Request);

#[cfg(any(feature = "pprof", feature = "otel"))]
type Pairs<'a> = TryVec<(&'a str, &'a str), Global>;

#[cfg(any(feature = "pprof", feature = "otel"))]
/// # Safety
///
/// `tags` points to `len` valid tags.
unsafe fn pairs<'a>(tags: *const Tag, len: usize) -> Result<Pairs<'a>, Status> {
    // SAFETY: forwarded.
    let tags = unsafe { slice(tags, len)? };
    let mut out =
        TryVec::try_with_capacity_in(tags.len(), Global).map_err(|_| Status::OutOfMemory)?;
    for tag in tags {
        // SAFETY: forwarded.
        let pair = unsafe { (text(tag.key)?, text(tag.value)?) };
        let _ = out.push_within_capacity(pair);
    }
    Ok(out)
}

#[cfg(any(feature = "pprof", feature = "otel"))]
/// # Safety
///
/// `resource` is valid and its strings outlive `'a`.
unsafe fn resource<'a>(
    resource: &Resource,
    tags: &'a [(&'a str, &'a str)],
) -> Result<CoreResource<'a>, Status> {
    // SAFETY: forwarded.
    unsafe {
        Ok(CoreResource {
            service: text(resource.service)?,
            env: text(resource.env)?,
            version: text(resource.version)?,
            host_name: text(resource.host_name)?,
            runtime_id: text(resource.runtime_id)?,
            language: text(resource.language)?,
            runtime_name: text(resource.runtime_name)?,
            runtime_version: text(resource.runtime_version)?,
            runtime_description: text(resource.runtime_description)?,
            profiler_version: text(resource.profiler_version)?,
            container_id: text(resource.container_id)?,
            cgroup_inode: resource.cgroup_inode,
            process_id: resource.process_id,
            tags,
        })
    }
}

/// How to encode a request: shared by every request builder.
#[cfg(any(feature = "pprof", feature = "otel"))]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EncodeOptions {
    /// The largest encoded (uncompressed) profile; 0 means 20 MiB.
    pub max_bytes: usize,
    /// `AUTO` lets the format choose: for pprof zstd when built in, else gzip
    /// when built in, else none; for OTLP none.
    pub compression: Compression,
}

/// Reads optional encode options: NULL means the defaults.
///
/// # Safety
///
/// `options` is NULL or valid.
#[cfg(any(feature = "pprof", feature = "otel"))]
unsafe fn encode_options(options: *const EncodeOptions) -> (CoreEncodeOptions, Compression) {
    // SAFETY: the caller's contract.
    let Some(o) = (unsafe { options.as_ref() }) else {
        return (CoreEncodeOptions::default(), Compression::Auto);
    };
    let core = if o.max_bytes == 0 {
        CoreEncodeOptions::default()
    } else {
        CoreEncodeOptions {
            max_bytes: o.max_bytes,
        }
    };
    (core, o.compression)
}

#[cfg(feature = "pprof")]
/// Lowers the profile, ending at `end_ns`, to a Datadog intake request.
///
/// The request is pprof plus `event.json`. `encode` (NULL for defaults) sets
/// the size limit, and its compression applies to the profile and
/// attachments; `AUTO` means zstd when built in, else gzip when built in,
/// else none.
///
/// # Safety
///
/// `profile` is a live profile not used concurrently; `resource` and
/// `options` and what they point to are valid; `out` is valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_datadog_request(
    profile: *const Profile,
    end_ns: i64,
    resource_ptr: *const Resource,
    options: *const DatadogOptions,
    encode: *const EncodeOptions,
    out: *mut *mut Request,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let (core_encode, compression) = unsafe { encode_options(encode) };
        // SAFETY: the caller's contract.
        let (profile, res, options) = unsafe {
            (
                profile.as_ref().ok_or(Status::InvalidArgument)?,
                resource_ptr.as_ref().ok_or(Status::InvalidArgument)?,
                options.as_ref().ok_or(Status::InvalidArgument)?,
            )
        };
        // SAFETY: the caller's contract.
        let tags = unsafe { pairs(res.tags, res.tags_len)? };
        // SAFETY: the caller's contract.
        let core_resource = unsafe { resource(res, &tags)? };
        // SAFETY: the caller's contract.
        let attachments = unsafe { slice(options.attachments, options.attachments_len)? };
        let mut files: TryVec<(&str, &[u8]), Global> =
            TryVec::try_with_capacity_in(attachments.len(), Global)
                .map_err(|_| Status::OutOfMemory)?;
        for attachment in attachments {
            // SAFETY: the caller's contract.
            let file = unsafe {
                (
                    text(attachment.name)?,
                    slice(attachment.bytes.ptr, attachment.bytes.len)?,
                )
            };
            let _ = files.push_within_capacity(file);
        }
        // SAFETY: the caller's contract.
        let core_options = unsafe {
            CoreDatadogOptions {
                library_name: text(options.library_name)?,
                library_version: text(options.library_version)?,
                family: text(options.family)?,
                internal_json: text(options.internal_json)?,
                info_json: text(options.info_json)?,
                process_tags: text(options.process_tags)?,
                attachments: &files,
                period: period(options.period)?,
            }
        };
        let request = request::datadog(
            &profile.profile,
            end_ns,
            &core_resource,
            &core_options,
            core_encode,
            compression,
        )?;
        try_box(Request(request))
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(request) => unsafe { put(out, request.as_ptr()) },
        Err(status) => status,
    }
}

#[cfg(feature = "otel")]
/// One OTLP `Profile` to write: the profile's sample type `source`, as the
/// sanctioned `sample_type`, with an optional period (NULL for none).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OtlpProfile {
    pub source: u32,
    pub sample_type: OtlpSampleType,
    pub period: *const Period,
}

#[cfg(feature = "otel")]
/// Lowers the profile, ending at `end_ns`, to an OTLP/HTTP profiles request.
///
/// The request has one OTLP profile per entry of `profiles`, and nothing
/// else. It is compressed as `encode` says (`Content-Encoding` is set).
///
/// # Safety
///
/// As for [`ddog_prof_Profile_datadog_request`], with `scope` for `options`;
/// `profiles` points to `profiles_len` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_otlp_request(
    profile: *const Profile,
    end_ns: i64,
    resource_ptr: *const Resource,
    scope: *const Scope,
    profiles: *const OtlpProfile,
    profiles_len: usize,
    encode: *const EncodeOptions,
    out: *mut *mut Request,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let (core_encode, compression) = unsafe { encode_options(encode) };
        // SAFETY: the caller's contract.
        let (profile, res, scope) = unsafe {
            (
                profile.as_ref().ok_or(Status::InvalidArgument)?,
                resource_ptr.as_ref().ok_or(Status::InvalidArgument)?,
                scope.as_ref().ok_or(Status::InvalidArgument)?,
            )
        };
        // SAFETY: the caller's contract.
        let tags = unsafe { pairs(res.tags, res.tags_len)? };
        // SAFETY: the caller's contract.
        let core_resource = unsafe { resource(res, &tags)? };
        // SAFETY: the caller's contract.
        let attributes = unsafe { pairs(scope.attributes, scope.attributes_len)? };
        // SAFETY: the caller's contract.
        let core_scope = unsafe {
            CoreScope {
                name: text(scope.name)?,
                version: text(scope.version)?,
                attributes: &attributes,
            }
        };
        // SAFETY: the caller's contract.
        let profiles = unsafe { slice(profiles, profiles_len)? };
        let mut specs = TryVec::try_with_capacity_in(profiles.len(), Global)
            .map_err(|_| Status::OutOfMemory)?;
        for p in profiles {
            let _ = specs.push_within_capacity(ProfileSpec {
                source: SampleTypeId::from_index(p.source),
                sample_type: p.sample_type,
                // SAFETY: the caller's contract.
                period: unsafe { period(p.period)? },
            });
        }
        let request = request::otlp(
            &profile.profile,
            end_ns,
            &core_resource,
            &core_scope,
            &specs,
            core_encode,
            compression,
        )?;
        try_box(Request(request))
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(request) => unsafe { put(out, request.as_ptr()) },
        Err(status) => status,
    }
}

#[cfg(feature = "pprof")]
/// Encodes the profile as pprof, without the intake envelope, for a file.
///
/// It is compressed as `encode` says (AUTO as for the Datadog request). Read
/// it with the `ddog_prof_Request_body` of an otherwise empty request.
///
/// # Safety
///
/// As for [`ddog_prof_Profile_datadog_request`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Profile_encode_pprof(
    profile: *const Profile,
    end_ns: i64,
    period_ptr: *const Period,
    encode: *const EncodeOptions,
    out: *mut *mut Request,
) -> Status {
    let result = (|| {
        // SAFETY: the caller's contract.
        let (core_encode, compression) = unsafe { encode_options(encode) };
        // SAFETY: the caller's contract.
        let profile = unsafe { profile.as_ref() }.ok_or(Status::InvalidArgument)?;
        let body = libdd_profiling::encode::pprof::encode(
            &profile.profile,
            end_ns,
            // SAFETY: the caller's contract.
            unsafe { period(period_ptr)? },
            core_encode,
        )?;
        let body = libdd_profiling::compress::compress(body, compression.for_pprof())?;
        try_box(Request(request::Request {
            method: "",
            path: "",
            headers: TryVec::new_in(Global),
            body,
        }))
    })();
    match result {
        // SAFETY: the caller's contract.
        Ok(request) => unsafe { put(out, request.as_ptr()) },
        Err(status) => status,
    }
}

/// # Safety
///
/// `request` is NULL or a live request.
unsafe fn request_ref<'a>(request: *const Request) -> Option<&'a request::Request> {
    // SAFETY: the caller's contract.
    unsafe { request.as_ref() }.map(|r| &r.0)
}

/// The request's method, such as `POST`. Empty for NULL.
///
/// # Safety
///
/// `request` is NULL or a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_method(request: *const Request) -> Str {
    // SAFETY: forwarded.
    Str::of(unsafe { request_ref(request) }.map_or("", |r| r.method))
}

/// The request's path, relative to the destination's base URL.
///
/// # Safety
///
/// `request` is NULL or a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_path(request: *const Request) -> Str {
    // SAFETY: forwarded.
    Str::of(unsafe { request_ref(request) }.map_or("", |r| r.path))
}

/// How many headers the request has.
///
/// # Safety
///
/// `request` is NULL or a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_header_count(request: *const Request) -> usize {
    // SAFETY: forwarded.
    unsafe { request_ref(request) }.map_or(0, |r| r.headers.len())
}

/// Header `index`'s name and value; they live as long as the request.
///
/// # Safety
///
/// `request` is a live request; `name` and `value` are valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_header(
    request: *const Request,
    index: usize,
    name: *mut Str,
    value: *mut Str,
) -> Status {
    // SAFETY: forwarded.
    let Some(header) = (unsafe { request_ref(request) }).and_then(|r| r.headers.get(index)) else {
        return Status::InvalidArgument;
    };
    if name.is_null() || value.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: non-null and valid by the caller's contract.
    unsafe {
        name.write(Str::of(header.name));
        value.write(Str {
            ptr: header.value.as_ptr(),
            len: header.value.len(),
        });
    }
    Status::Ok
}

/// The request body; it lives as long as the request.
///
/// # Safety
///
/// `request` is NULL or a live request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_body(request: *const Request) -> Bytes {
    // SAFETY: forwarded.
    unsafe { request_ref(request) }.map_or(
        Bytes {
            ptr: ptr::null(),
            len: 0,
        },
        |r| Bytes {
            ptr: r.body.as_ptr(),
            len: r.body.len(),
        },
    )
}

/// Frees a request.
///
/// # Safety
///
/// `request` is NULL or a live request, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_Request_drop(request: *mut Request) {
    if let Some(request) = NonNull::new(request) {
        // SAFETY: the caller hands over the request.
        unsafe { drop_box(request) };
    }
}

/// The negative control for the no-panic check: an index the optimizer
/// cannot prove in bounds. Built only with `--cfg ddprof_ffi_canary`.
///
/// # Safety
///
/// `bytes` points to `len` bytes.
#[cfg(ddprof_ffi_canary)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_prof_canary(bytes: *const u8, len: usize, index: usize) -> u8 {
    // SAFETY: the caller's contract.
    let bytes = unsafe { core::slice::from_raw_parts(bytes, len) };
    bytes[index]
}
