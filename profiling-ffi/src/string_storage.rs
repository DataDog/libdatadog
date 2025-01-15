// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use datadog_profiling::collections::string_storage::ManagedStringStorage as InternalManagedStringStorage;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::{CharSlice, Error, MaybeError, StringWrapper};
use libc::c_void;
use std::num::NonZeroU32;
use std::{rc::Rc, sync::RwLock};

#[repr(C)]
pub struct ManagedStringId {
    pub value: u32,
}

#[repr(C)]
pub struct ManagedStringStorage {
    // This may be null, but if not it will point to a valid InternalManagedStringStorage.
    inner: *const c_void, /* Actually *RwLock<InternalManagedStringStorage> but cbindgen doesn't
                           * opaque RwLock */
}

#[allow(dead_code)]
#[repr(C)]
pub enum ManagedStringStorageNewResult {
    Ok(ManagedStringStorage),
    #[allow(dead_code)]
    Err(Error),
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_new() -> ManagedStringStorageNewResult {
    let storage = InternalManagedStringStorage::new();

    ManagedStringStorageNewResult::Ok(ManagedStringStorage {
        inner: Rc::into_raw(Rc::new(RwLock::new(storage))) as *const c_void,
    })
}

#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_drop(storage: ManagedStringStorage) {
    if let Ok(storage) = get_inner_string_storage(storage, false) {
        drop(storage);
    }
}

#[repr(C)]
#[allow(dead_code)]
pub enum ManagedStringStorageInternResult {
    Ok(ManagedStringId),
    Err(Error),
}

#[must_use]
#[no_mangle]
/// TODO: Consider having a variant of intern (and unintern?) that takes an array as input, instead
/// of just a single string at a time.
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_intern(
    storage: ManagedStringStorage,
    string: CharSlice,
) -> ManagedStringStorageInternResult {
    // Empty strings always get assigned id 0, no need to check.
    if string.len() == 0 {
        return anyhow::Ok(ManagedStringId { value: 0 }).into();
    }

    (|| {
        let storage = get_inner_string_storage(storage, true)?;

        let string_id = storage
            .write()
            .map_err(|_| {
                anyhow::anyhow!("acquisition of write lock on string storage should succeed")
            })?
            .intern(string.try_to_utf8()?)?;

        anyhow::Ok(ManagedStringId { value: string_id })
    })()
    .context("ddog_prof_ManagedStringStorage_intern failed")
    .into()
}

#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_unintern(
    storage: ManagedStringStorage,
    id: ManagedStringId,
) -> MaybeError {
    let non_empty_string_id = if let Some(valid_id) = NonZeroU32::new(id.value) {
        valid_id
    } else {
        return MaybeError::None; // Empty string, nothing to do
    };

    let result = (|| {
        let storage = get_inner_string_storage(storage, true)?;
        let read_locked_storage = storage.read().map_err(|_| {
            anyhow::anyhow!("acquisition of read lock on string storage should succeed")
        })?;
        read_locked_storage.unintern(non_empty_string_id)
    })()
    .context("ddog_prof_ManagedStringStorage_unintern failed");

    match result {
        Ok(_) => MaybeError::None,
        Err(e) => MaybeError::Some(e.into()),
    }
}

#[repr(C)]
#[allow(dead_code)]
pub enum StringWrapperResult {
    Ok(StringWrapper),
    Err(Error),
}

#[must_use]
#[no_mangle]
/// Returns a string given its id.
/// This API is mostly for testing, overall you should avoid reading back strings from libdatadog
/// once they've been interned and should instead always operate on the id.
/// Remember to `ddog_StringWrapper_drop` the string once you're done with it.
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_get_string(
    storage: ManagedStringStorage,
    id: ManagedStringId,
) -> StringWrapperResult {
    (|| {
        let storage = get_inner_string_storage(storage, true)?;
        let string: String = (*storage
            .read()
            .map_err(|_| {
                anyhow::anyhow!("acquisition of read lock on string storage should succeed")
            })?
            .get_string(id.value)?)
        .to_owned();

        anyhow::Ok(string)
    })()
    .context("ddog_prof_ManagedStringStorage_get_string failed")
    .into()
}

#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_advance_gen(
    storage: ManagedStringStorage,
) -> MaybeError {
    let result = (|| {
        let storage = get_inner_string_storage(storage, true)?;

        storage
            .write()
            .map_err(|_| {
                anyhow::anyhow!("acquisition of write lock on string storage should succeed")
            })?
            .advance_gen();

        anyhow::Ok(())
    })()
    .context("ddog_prof_ManagedStringStorage_advance_gen failed");

    match result {
        Ok(_) => MaybeError::None,
        Err(e) => MaybeError::Some(e.into()),
    }
}

pub unsafe fn get_inner_string_storage(
    storage: ManagedStringStorage,
    cloned: bool,
) -> anyhow::Result<Rc<RwLock<InternalManagedStringStorage>>> {
    if storage.inner.is_null() {
        anyhow::bail!("storage inner pointer is null");
    }

    let storage_ptr = storage.inner;

    if cloned {
        // By incrementing strong count here we ensure that the returned Rc represents a "clone" of
        // the original and will thus not trigger a drop of the underlying data when out of
        // scope. NOTE: We can't simply do Rc::from_raw(storage_ptr).clone() because when we
        // return, the Rc created through `Rc::from_raw` would go out of scope and decrement
        // strong count.
        Rc::increment_strong_count(storage_ptr);
    }
    Ok(Rc::from_raw(
        storage_ptr as *const RwLock<InternalManagedStringStorage>,
    ))
}

impl From<anyhow::Result<ManagedStringId>> for ManagedStringStorageInternResult {
    fn from(value: anyhow::Result<ManagedStringId>) -> Self {
        match value {
            Ok(v) => Self::Ok(v),
            Err(err) => Self::Err(err.into()),
        }
    }
}

impl From<anyhow::Result<String>> for StringWrapperResult {
    fn from(value: anyhow::Result<String>) -> Self {
        match value {
            Ok(v) => Self::Ok(v.into()),
            Err(err) => Self::Err(err.into()),
        }
    }
}
