// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus;
use datadog_profiling::profiles::collections::SetId;
use datadog_profiling::profiles::datatypes::{Mapping, MappingId};
use datadog_profiling::profiles::{datatypes, ProfileError};
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

/// Opaque handle type for a mapping set. Do not reach into this, it's only
/// there for size and alignment and the detail may change.
pub type MappingSet = *mut ();

/// Tries to create a new mapping set.
/// If the status is OK, then the `set` has been written with an actual set
/// handle which will later need to be dropped; otherwise it remains
/// unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_MappingSet_new(
    set: NonNull<MappingSet>,
) -> ProfileStatus {
    match datatypes::MappingSet::try_new() {
        Ok(mapping_set) => {
            unsafe { set.write(mapping_set.into_raw().as_ptr()) };
            ProfileStatus::OK
        }
        Err(err) => ProfileStatus::from_error(err),
    }
}

fn mapping_set_insert(
    set: MappingSet,
    mapping: Mapping,
) -> Result<SetId<Mapping>, ProfileError> {
    match NonNull::new(set) {
        None => Err(ProfileError::InvalidInput),
        Some(raw) => {
            let mapping_set = ManuallyDrop::new(unsafe {
                datatypes::MappingSet::from_raw(raw)
            });
            match mapping_set.try_insert(mapping) {
                Ok(id) => Ok(id),
                Err(err) => Err(err.into()),
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_MappingSet_insert(
    id: NonNull<MappingId>,
    set: MappingSet,
    mapping: Mapping,
) -> ProfileStatus {
    ProfileStatus::from(
        mapping_set_insert(set, mapping)
            .map(|mapping_id| unsafe { id.write(mapping_id.cast()) }),
    )
}

unsafe fn mapping_set_get(
    set: MappingSet,
    mapping_id: MappingId,
) -> Result<Mapping, ProfileError> {
    match NonNull::new(set) {
        None => Err(ProfileError::InvalidInput),
        Some(raw) => {
            let mapping_set = ManuallyDrop::new(unsafe {
                datatypes::MappingSet::from_raw(raw)
            });
            Ok(unsafe { *mapping_set.get(mapping_id.cast()) })
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_MappingSet_get(
    mapping: NonNull<Mapping>,
    set: MappingSet,
    mapping_id: MappingId,
) -> ProfileStatus {
    ProfileStatus::from(
        mapping_set_get(set, mapping_id)
            .map(|map| unsafe { mapping.write(map) }),
    )
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof_MappingSet_drop(set: MappingSet) {
    if let Some(raw) = NonNull::new(set) {
        drop(unsafe { datatypes::MappingSet::from_raw(raw) });
    }
}
