use anyhow::Context;
use libc::c_void;
use std::{ffi::CStr, rc::Rc, sync::RwLock};

use datadog_profiling::collections::string_storage::ManagedStringStorage as InternalManagedStringStorage;
use ddcommon_ffi::{CharSlice, Error, StringWrapper};

#[repr(C)]
pub struct ManagedStringStorage {
    // This may be null, but if not it will point to a valid Profile.
    inner: *const c_void, /* Actually *RwLock<InternalManagedStringStorage> but cbindgen doesn't
                           * opaque RwLock */
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_new() -> ManagedStringStorage {
    let storage = InternalManagedStringStorage::new();

    ManagedStringStorage {
        inner: Rc::into_raw(Rc::new(RwLock::new(storage))) as *const c_void,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_drop(storage: ManagedStringStorage) {
    if let Ok(storage) = get_inner_string_storage(storage, false) {
        drop(storage);
    }
}

#[repr(C)]
#[allow(dead_code)]
pub enum ManagedStringStorageInternResult {
    Ok(u32),
    Err(Error),
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_intern(
    storage: ManagedStringStorage,
    string: Option<&CharSlice>,
) -> ManagedStringStorageInternResult {
    (|| {
        let storage = get_inner_string_storage(storage, true)?;

        let string: &CharSlice = string.expect("non null string");
        let string: &str = CStr::from_ptr(string.as_ptr())
            .to_str()
            .expect("valid utf8 string");

        let string_id = storage
            .write()
            .expect("acquisition of write lock on string storage should succeed")
            .intern(string);

        anyhow::Ok(string_id)
    })()
    .context("ddog_prof_Profile_serialize failed")
    .into()
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_unintern(
    storage: ManagedStringStorage,
    id: u32,
) -> ManagedStringStorageResult {
    (|| {
        let storage = get_inner_string_storage(storage, true)?;
        storage
            .read()
            .expect("acquisition of read lock on string storage should succeed")
            .unintern(id);
        anyhow::Ok(())
    })()
    .context("ddog_prof_Profile_serialize failed")
    .into()
}

#[repr(C)]
#[allow(dead_code)]
pub enum StringWrapperResult {
    Ok(StringWrapper),
    Err(Error),
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_get_string(
    storage: ManagedStringStorage,
    id: u32,
) -> StringWrapperResult {
    (|| {
        let storage = get_inner_string_storage(storage, true)?;
        let string: String = (*storage
            .read()
            .expect("acquisition of read lock on string storage should succeed")
            .get_string(id))
        .to_owned();

        anyhow::Ok(string)
    })()
    .context("ddog_prof_Profile_serialize failed")
    .into()
}

#[repr(C)]
#[allow(dead_code)]
pub enum ManagedStringStorageResult {
    Ok(()),
    Err(Error),
}

#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ManagedStringStorage_advance_gen(
    storage: ManagedStringStorage,
) -> ManagedStringStorageResult {
    (|| {
        let storage = get_inner_string_storage(storage, true)?;

        storage
            .write()
            .expect("acquisition of write lock on string storage should succeed")
            .advance_gen();

        anyhow::Ok(())
    })()
    .context("ddog_prof_Profile_serialize failed")
    .into()
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

impl From<anyhow::Result<u32>> for ManagedStringStorageInternResult {
    fn from(value: anyhow::Result<u32>) -> Self {
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

impl From<anyhow::Result<()>> for ManagedStringStorageResult {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(v) => Self::Ok(v),
            Err(err) => Self::Err(err.into()),
        }
    }
}
