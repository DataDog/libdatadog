// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use libdd_profiling::api::ManagedStringId;
use libdd_profiling::collections::string_storage::ManagedStringStorage as InternalManagedStringStorage;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::{CharSlice, Error, MaybeError, Slice, StringWrapperResult};
use libc::c_void;
use std::mem::MaybeUninit;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::Mutex;

// A note about this being Copy:
// We're writing code for C with C semantics but with Rust restrictions still
// around. In terms of C, this is just a pointer with some unknown lifetime
// that is the programmer's job to handle.
// Normally, Rust is taking care of that lifetime for us. Because we need to
// uncouple this so that lifetimes can bridge C and Rust, the lifetime of the
// object isn't managed in Rust, but in the sequence of API calls.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct ManagedStringStorage {
    // This may be null, but if not it will point to a valid InternalManagedStringStorage,
    // wrapped as needed for correct concurrency. This type is made opaque for cbindgen.
    inner: *const c_void,
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
pub extern "C" fn ddog_prof_ManagedStringStorage_new() -> ManagedStringStorageNewResult {
    let storage = InternalManagedStringStorage::new();

    ManagedStringStorageNewResult::Ok(ManagedStringStorage {
        inner: Arc::into_raw(Arc::new(Mutex::new(storage))) as *const c_void,
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
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_intern(
    storage: ManagedStringStorage,
    string: CharSlice,
) -> ManagedStringStorageInternResult {
    // Empty strings always get assigned id 0, no need to check.
    if string.is_empty() {
        return anyhow::Ok(ManagedStringId::empty()).into();
    }

    (|| {
        let storage = get_inner_string_storage(storage, true)?;

        let string_id = storage
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?
            .intern(string.try_to_utf8()?)?;

        anyhow::Ok(ManagedStringId::new(string_id))
    })()
    .context("ddog_prof_ManagedStringStorage_intern failed")
    .into()
}

/// Interns all the strings in `strings`, writing the resulting id to the same
/// offset in `output_ids`.
///
/// This can fail if:
///  1. The given `output_ids_size` doesn't match the size of the input slice.
///  2. The internal storage pointer is null.
///  3. It fails to acquire a lock (e.g. it was poisoned).
///  4. Defensive checks against bugs fail.
///
/// If a failure occurs, do not use any of the ids in the output array. After
/// this point, you should only use read-only routines (except for drop) on
/// the managed string storage.
#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_intern_all(
    storage: ManagedStringStorage,
    strings: Slice<CharSlice>,
    output_ids: *mut MaybeUninit<ManagedStringId>,
    output_ids_size: usize,
) -> MaybeError {
    let result = (|| {
        if strings.len() != output_ids_size {
            anyhow::bail!("input and output arrays have different sizes")
        }

        let storage = get_inner_string_storage(storage, true)?;

        let mut write_locked_storage = storage
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?;

        let output_slice = core::slice::from_raw_parts_mut(output_ids, output_ids_size);

        for (output_id, input_str) in output_slice.iter_mut().zip(strings.iter()) {
            let string_id = if input_str.is_empty() {
                ManagedStringId::empty()
            } else {
                ManagedStringId::new(write_locked_storage.intern(input_str.try_to_utf8()?)?)
            };
            output_id.write(string_id);
        }

        anyhow::Ok(())
    })()
    .context("ddog_prof_ManagedStringStorage_intern failed");

    match result {
        Ok(_) => MaybeError::None,
        Err(e) => MaybeError::Some(e.into()),
    }
}

#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_unintern(
    storage: ManagedStringStorage,
    id: ManagedStringId,
) -> MaybeError {
    let Some(non_empty_string_id) = NonZeroU32::new(id.value) else {
        return MaybeError::None; // Empty string, nothing to do
    };

    let result = (|| {
        let storage = get_inner_string_storage(storage, true)?;

        let mut write_locked_storage = storage
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?;

        write_locked_storage.unintern(non_empty_string_id)
    })()
    .context("ddog_prof_ManagedStringStorage_unintern failed");

    match result {
        Ok(_) => MaybeError::None,
        Err(e) => MaybeError::Some(e.into()),
    }
}

#[no_mangle]
/// TODO: @ivoanjo Should this take a `*mut ManagedStringStorage` like Profile APIs do?
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_unintern_all(
    storage: ManagedStringStorage,
    ids: Slice<ManagedStringId>,
) -> MaybeError {
    let result = (|| {
        let storage = get_inner_string_storage(storage, true)?;

        let mut write_locked_storage = storage
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?;

        for non_empty_string_id in ids.iter().filter_map(|id| NonZeroU32::new(id.value)) {
            write_locked_storage.unintern(non_empty_string_id)?;
        }

        anyhow::Ok(())
    })()
    .context("ddog_prof_ManagedStringStorage_unintern failed");

    match result {
        Ok(_) => MaybeError::None,
        Err(e) => MaybeError::Some(e.into()),
    }
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
            .lock()
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
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?
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
    // This should be `true` in every case EXCEPT when implementing `drop`, which uses `false`.
    // (E.g. we use this flag to know if we need to increment the refcount for the copy we create
    // or not).
    for_use: bool,
) -> anyhow::Result<Arc<Mutex<InternalManagedStringStorage>>> {
    if storage.inner.is_null() {
        anyhow::bail!("storage inner pointer is null");
    }

    let storage_ptr = storage.inner;

    if for_use {
        // By incrementing strong count here we ensure that the returned Arc represents a "clone" of
        // the original and will thus not trigger a drop of the underlying data when out of
        // scope. NOTE: We can't simply do Arc::from_raw(storage_ptr).clone() because when we
        // return, the Arc created through `Arc::from_raw` would go out of scope and decrement
        // strong count.
        Arc::increment_strong_count(storage_ptr);
    }
    Ok(Arc::from_raw(
        storage_ptr as *const Mutex<InternalManagedStringStorage>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_storage() {
        let storage = match ddog_prof_ManagedStringStorage_new() {
            ManagedStringStorageNewResult::Ok(ok) => ok,
            ManagedStringStorageNewResult::Err(err) => panic!("{err}"),
        };
        let string_rs = [
            CharSlice::from("I'm running out of time."),
            CharSlice::from("My zoom meeting ends in 2 minutes."),
        ];
        let strings = Slice::new(&string_rs);

        // We're going to intern the same group of strings twice to make sure
        // that we get the same ids.
        let mut ids_rs1 = [ManagedStringId::empty(); 2];
        let ids1 = ids_rs1.as_mut_ptr();
        let result = unsafe {
            ddog_prof_ManagedStringStorage_intern_all(storage, strings, ids1.cast(), strings.len())
        };
        if let MaybeError::Some(err) = result {
            panic!("{err}");
        }

        let mut ids_rs2 = [ManagedStringId::empty(); 2];
        let ids2 = ids_rs2.as_mut_ptr();
        let result = unsafe {
            ddog_prof_ManagedStringStorage_intern_all(storage, strings, ids2.cast(), strings.len())
        };
        if let MaybeError::Some(err) = result {
            panic!("{err}");
        }

        // Check the ids match and aren't zero.
        {
            assert_eq!(ids_rs1, ids_rs2);
            for id in ids_rs1 {
                assert_ne!(id.value, 0);
            }
        }

        unsafe { ddog_prof_ManagedStringStorage_drop(storage) }
    }
}
