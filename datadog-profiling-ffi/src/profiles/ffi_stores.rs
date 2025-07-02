// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use datadog_profiling::collections::{Storable, Store};
use datadog_profiling_protobuf::*;

use std::ptr;

trait FfiStore: Default {
    type Storable: Storable + Copy + Value;
}

impl<T: Storable + Copy + Value> FfiStore for Store<T> {
    type Storable = T;
}

fn ffi_new<S: FfiStore>() -> *mut S {
    match datadog_alloc::Box::try_new(S::default()) {
        Ok(boxed) => datadog_alloc::Box::into_raw(boxed),
        Err(_err) => ptr::null_mut(),
    }
}

fn ffi_store_insert<S: FfiStore>(
    store: *mut S,
    data: S::Storable,
) -> StoreInsertResult {
    let ptr = store.cast::<Store<S::Storable>>();
    let Some(store) = (unsafe { ptr.as_mut() }) else {
        return StoreInsertResult::Err(ProfileError::InvalidInput);
    };

    match store.insert(data) {
        Ok(id) => StoreInsertResult::Ok(id),
        Err(_err) => StoreInsertResult::Err(ProfileError::OutOfMemory),
    }
}

unsafe fn ffi_store_clear<S: FfiStore>(store: *mut S) {
    let ptr = store.cast::<Store<S::Storable>>();
    if let Some(store) = ptr.as_mut() {
        store.clear();
    }
}

/// # Safety
///
/// The inner pointer must point to a valid store object if it is not null.
unsafe fn ffi_store_drop<S: FfiStore>(store: *mut *mut S) {
    if let Some(ptr) = store.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(datadog_alloc::Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}

/// A result for operations such as [`ddog_prof_Store_Mapping_insert`].
#[repr(C)]
#[derive(Debug)]
pub enum StoreInsertResult {
    /// The id of the stored item.
    Ok(u64),
    Err(ProfileError),
}

impl From<StoreInsertResult> for Result<u64, ProfileError> {
    fn from(result: StoreInsertResult) -> Self {
        match result {
            StoreInsertResult::Ok(id) => Ok(id),
            StoreInsertResult::Err(err) => Err(err),
        }
    }
}

/// Tries to create a new, empty mapping store.
///
/// # Errors
///
/// Fails if memory cannot be allocated for the store.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_Store_Mapping_new() -> *mut Store<Mapping> {
    ffi_new()
}

/// # Safety
///
/// Pointer to the mapping store should be valid.
///
/// # Examples
///
/// ```
/// # use datadog_profiling_ffi::Error;
/// # use datadog_profiling_ffi::profiles::*;
/// # use datadog_profiling_protobuf::*;
/// # use std::ptr::addr_of_mut;
/// # fn main() -> Result<(), ProfileError> { unsafe {
/// let mut mapping_store = ddog_prof_Store_Mapping_new();
/// if mapping_store.is_null() {
///     return Err(ProfileError::OutOfMemory);
/// }
/// let mapping = Mapping {
///     id: 1.into(),
///     ..Mapping::default()
/// };
/// let insert_result = Result::from(ddog_prof_Store_Mapping_insert(
///     mapping_store,
///     mapping,
/// ));
/// match insert_result {
///     Ok(id) => {}    // do something with id
///     Err(_err) => {} // report or record error
/// }
/// ddog_prof_Store_Mapping_drop(addr_of_mut!(mapping_store));
/// # } Ok(()) }
/// ```
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Store_Mapping_insert(
    store: *mut Store<Mapping>,
    mapping: Mapping,
) -> StoreInsertResult {
    ffi_store_insert(store, mapping)
}

/// # Safety
///
/// The `store` must be a valid pointer to a pointer to a `Store<Mapping>`.
/// `*store` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Mapping_drop(
    store: *mut *mut Store<Mapping>,
) {
    ffi_store_drop(store);
}

/// # Safety
///
/// The `store` must be a valid pointer to a `Store<Mapping>` if not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Mapping_clear(
    store: *mut Store<Mapping>,
) {
    ffi_store_clear(store);
}

/// Tries to create a new, empty location store.
///
/// # Errors
///
/// Fails if memory cannot be allocated for the store.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_Store_Location_new() -> *mut Store<Location> {
    ffi_new()
}

/// # Safety
///
/// Pointer to the location store should be valid.
///
/// # Examples
///
/// ```
/// # use datadog_profiling_ffi::Error;
/// # use datadog_profiling_ffi::profiles::*;
/// # use datadog_profiling_protobuf::*;
/// # use std::ptr::addr_of_mut;
/// # fn main() -> Result<(), ProfileError> { unsafe {
/// let mut location_store = ddog_prof_Store_Location_new();
/// if location_store.is_null() {
///     return Err(ProfileError::OutOfMemory);
/// }
/// let location = Location {
///     id: 1.into(),
///     ..Location::default()
/// };
/// let insert_result = Result::from(ddog_prof_Store_Location_insert(
///     location_store,
///     location,
/// ));
/// match insert_result {
///     Ok(id) => {}    // do something with id
///     Err(_err) => {} // report or record error
/// }
/// ddog_prof_Store_Location_drop(addr_of_mut!(location_store));
/// # } Ok(()) }
/// ```
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Store_Location_insert(
    store: *mut Store<Location>,
    location: Location,
) -> StoreInsertResult {
    ffi_store_insert(store, location)
}

/// # Safety
///
/// The `store` must be a valid pointer to a pointer to a `Store<Location>`.
/// `*store` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Location_drop(
    store: *mut *mut Store<Location>,
) {
    ffi_store_drop(store);
}

/// # Safety
///
/// The `store` must be a valid pointer to a `Store<Location>` if not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Location_clear(
    store: *mut Store<Location>,
) {
    ffi_store_clear(store);
}

/// Tries to create a new, empty function store.
///
/// # Errors
///
/// Fails if memory cannot be allocated for the store.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_Store_Function_new() -> *mut Store<Function> {
    ffi_new()
}

/// # Safety
///
/// Pointer to the function store should be valid.
///
/// # Examples
///
/// ```
/// # use datadog_profiling_ffi::Error;
/// # use datadog_profiling_ffi::profiles::*;
/// # use datadog_profiling_protobuf::*;
/// # use std::ptr::addr_of_mut;
/// # fn main() -> Result<(), ProfileError> { unsafe {
/// let mut function_store = ddog_prof_Store_Function_new();
/// if function_store.is_null() {
///     return Err(ProfileError::OutOfMemory);
/// }
/// let function = Function {
///     id: 1.into(),
///     ..Function::default()
/// };
/// let insert_result = Result::from(ddog_prof_Store_Function_insert(
///     function_store,
///     function,
/// ));
/// match insert_result {
///     Ok(id) => {}    // do something with id
///     Err(_err) => {} // report or record error
/// }
/// ddog_prof_Store_Function_drop(addr_of_mut!(function_store));
/// # } Ok(()) }
/// ```
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Store_Function_insert(
    store: *mut Store<Function>,
    function: Function,
) -> StoreInsertResult {
    ffi_store_insert(store, function)
}

/// # Safety
///
/// The `store` must be a valid pointer to a pointer to a `Store<Function>`.
/// `*store` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Function_drop(
    store: *mut *mut Store<Function>,
) {
    ffi_store_drop(store);
}

/// # Safety
///
/// The `store` must be a valid pointer to a `Store<Function>` if not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Store_Function_clear(
    store: *mut Store<Function>,
) {
    ffi_store_clear(store);
}
