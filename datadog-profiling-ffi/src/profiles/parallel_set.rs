// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus;
use core::any::TypeId;
use core::hash;
use datadog_profiling::profiles::collections::{ParallelSet, SetId};
use datadog_profiling::profiles::datatypes::{Function, Location, Mapping};
use datadog_profiling::profiles::ProfileError;
use std::ffi::c_void;
use std::mem::{transmute, ManuallyDrop};
use std::ptr::NonNull;

// I know from the past that implementing ParallelSet<T> for every T is a pain,
// going to try a different approach with TypeId.
pub type FfiParallelSet = *mut FfiParallelSetImpl;

// Since TypeId is not FFI-safe, we have to double-box.
// todo: consider putting the type_id into the parallel set struct impl to
//       avoid double boxing.
pub struct FfiParallelSetImpl {
    type_id: TypeId,
    set: ManuallyDrop<ParallelSet<(), 4>>,
}

impl Drop for FfiParallelSetImpl {
    fn drop(&mut self) {
        if self.is::<Function>() {
            unsafe {
                drop(core::ptr::read(self.downcast_unchecked::<Function>()))
            };
        } else if self.is::<Location>() {
            unsafe {
                drop(core::ptr::read(self.downcast_unchecked::<Location>()))
            };
        } else if self.is::<Mapping>() {
            unsafe {
                drop(core::ptr::read(self.downcast_unchecked::<Mapping>()))
            };
        }
    }
}

impl FfiParallelSetImpl {
    fn try_new<T: hash::Hash + Eq + 'static>(
    ) -> Result<datadog_alloc::Box<FfiParallelSetImpl>, ProfileError> {
        let Ok(mut boxed) = datadog_alloc::Box::try_new_uninit() else {
            return Err(ProfileError::OutOfMemory);
        };
        match ParallelSet::<T, 4>::try_new() {
            Ok(set) => {
                boxed.write(FfiParallelSetImpl {
                    type_id: TypeId::of::<T>(),
                    set: ManuallyDrop::new(set.cast()),
                });
                Ok(unsafe { boxed.assume_init() })
            }
            Err(err) => Err(ProfileError::from(err)),
        }
    }

    unsafe fn downcast_unchecked<T: hash::Hash + Eq + 'static>(
        &self,
    ) -> &ParallelSet<T, 4> {
        unsafe { transmute(&self.set) }
    }

    fn is<T: hash::Hash + Eq + 'static>(&self) -> bool {
        self.type_id == TypeId::of::<T>()
    }

    fn insert(
        &self,
        t: NonNull<c_void>,
    ) -> Result<SetId<c_void>, ProfileError> {
        if self.is::<Function>() {
            let set = unsafe { self.downcast_unchecked::<Function>() };
            match set.try_insert(unsafe { t.cast::<Function>().read() }) {
                Ok(id) => Ok(id.cast()),
                Err(err) => Err(ProfileError::from(err)),
            }
        } else if self.is::<Location>() {
            let set = unsafe { self.downcast_unchecked::<Location>() };
            match set.try_insert(unsafe { t.cast::<Location>().read() }) {
                Ok(id) => Ok(id.cast()),
                Err(err) => Err(ProfileError::from(err)),
            }
        } else if self.is::<Mapping>() {
            let set = unsafe { self.downcast_unchecked::<Mapping>() };
            match set.try_insert(unsafe { t.cast::<Mapping>().read() }) {
                Ok(id) => Ok(id.cast()),
                Err(err) => Err(ProfileError::from(err)),
            }
        } else {
            Err(ProfileError::InvalidInput)
        }
    }
}

/// Tries to create a new mapping set.
/// If the status is OK, then the `set` has been written with an actual set
/// handle which will later need to be dropped; otherwise it remains
/// unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelSet_Function_new(
    set: NonNull<FfiParallelSet>,
) -> ProfileStatus {
    ProfileStatus::from(
        FfiParallelSetImpl::try_new::<Function>()
            .map(|ok| unsafe { set.write(datadog_alloc::Box::into_raw(ok)) }),
    )
}

/// Tries to create a new mapping set.
/// If the status is OK, then the `set` has been written with an actual set
/// handle which will later need to be dropped; otherwise it remains
/// unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelSet_Location_new(
    set: NonNull<FfiParallelSet>,
) -> ProfileStatus {
    ProfileStatus::from(
        FfiParallelSetImpl::try_new::<Location>()
            .map(|ok| unsafe { set.write(datadog_alloc::Box::into_raw(ok)) }),
    )
}

/// Tries to create a new mapping set.
/// If the status is OK, then the `set` has been written with an actual set
/// handle which will later need to be dropped; otherwise it remains
/// unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelSet_Mapping_new(
    set: NonNull<FfiParallelSet>,
) -> ProfileStatus {
    ProfileStatus::from(
        FfiParallelSetImpl::try_new::<Mapping>()
            .map(|ok| unsafe { set.write(datadog_alloc::Box::into_raw(ok)) }),
    )
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelSet_insert(
    id: Option<&mut NonNull<c_void>>,
    set: FfiParallelSet,
    item: NonNull<c_void>,
) -> ProfileStatus {
    let Some(id_ref) = id else {
        return ProfileStatus::from_error(ProfileError::InvalidInput);
    };
    let Some(set) = NonNull::new(set) else {
        return ProfileStatus::from_error(ProfileError::InvalidInput);
    };

    let set = unsafe { set.as_ref() };
    ProfileStatus::from(set.insert(item).map(|new_id| {
        *id_ref = new_id.into_raw();
    }))
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ParallelSet_drop(set: FfiParallelSet) {
    if set.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(set) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_profiling::profiles::collections::StringId;
    use datadog_profiling::profiles::datatypes::FunctionId;
    use std::borrow::Cow;
    use std::ffi::CStr;
    use std::ptr::null_mut;
    #[test]
    fn functions() {
        unsafe {
            let mut set = null_mut();
            let status =
                ddog_prof_ParallelSet_Function_new(NonNull::from(&mut set));
            if let Err(err) = Result::from(status) {
                ddog_prof_ParallelSet_drop(set);
                panic!("failed to create new set: {err:?}")
            }

            let item = Function {
                // not really a function, just testing a non-zero string.
                name: StringId::LOCAL_ROOT_SPAN_ID,
                ..Function::default()
            };
            let mut id = FunctionId::dangling();
            let status = ddog_prof_ParallelSet_insert(
                Some(&mut id),
                set,
                NonNull::from(&item).cast(),
            );
            if let Err(err) = Result::from(status) {
                ddog_prof_ParallelSet_drop(set);
                panic!("failed to insert: {err:?}")
            }
            ddog_prof_ParallelSet_drop(set);
        }
    }
}
