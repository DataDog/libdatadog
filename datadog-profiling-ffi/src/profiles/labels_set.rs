// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use datadog_alloc::Box;
use datadog_profiling::collections::{SliceSet, SliceSetInsertResult};
use datadog_profiling::profiles::SliceId;
use datadog_profiling_protobuf::Label;
use ddcommon_ffi::Slice;
use std::ptr;

/// Holds a set of labels. Labels are not sorted--the input order does matter.
pub type LabelsSet = SliceSet<Label>;

/// Creates a new, empty labels set.
///
/// # Errors
///
/// Returns null if allocation fails.
#[no_mangle]
pub extern "C" fn ddog_prof_LabelsSet_new() -> *mut LabelsSet {
    match Box::try_new(LabelsSet::new()) {
        Ok(boxed) => Box::into_raw(boxed),
        Err(_err) => ptr::null_mut(),
    }
}

/// Inserts a new label slice into the set.
///
/// # Errors
///
///  1. Fails if `set` is null.
///  2. Fails if `labels` is an invalid slice (note that there are still
///     safety conditions we cannot check at runtime that need to be ensured).
///  3. Fails if the `set` needs to grow and fails to allocate memory.
///  4. Fails if the `set` has grown too much and is full. It internally
///     compresses two `u32`s into a `u64`, so it fails if it grows too large.
///
/// # Safety
///
///  1. The `set` must be a valid reference if not null.
///  2. There must be no label slices from `ddog_prof_LabelsSet_lookup` still
///     alive when this is called.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_LabelsSet_insert(
    set: *mut LabelsSet,
    labels: Slice<Label>,
) -> SliceSetInsertResult {
    let Some(set) = set.as_mut() else {
        return SliceSetInsertResult::Err(ProfileError::InvalidInput);
    };
    let Some(labels) = labels.try_as_slice() else {
        return SliceSetInsertResult::Err(ProfileError::InvalidInput);
    };

    set.insert(labels).into()
}

#[repr(C)]
#[derive(Debug)]
pub enum LabelsSetLookupResult<'a> {
    Ok(Slice<'a, Label>),
    Err(ProfileError),
}

impl<'a> From<LabelsSetLookupResult<'a>>
    for Result<Slice<'a, Label>, ProfileError>
{
    fn from(result: LabelsSetLookupResult<'a>) -> Self {
        match result {
            LabelsSetLookupResult::Ok(ok) => Ok(ok),
            LabelsSetLookupResult::Err(err) => Err(err),
        }
    }
}

/// Finds the labels associated to the id.
///
/// # Safety
///
///  1. The set must be a valid reference and _must not be null_.
///  2. The lifetime of the returned slice is tied to the underlying storage
///     of the set. You must not clear, drop, or insert while the label slice
///     is alive.
///  3. The label id must not have been manipulated; it must be treated as
///     opaque storage.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_LabelsSet_lookup(
    label_set: &mut LabelsSet,
    id: SliceId,
) -> LabelsSetLookupResult {
    match label_set.lookup(id.into()) {
        Some(slice) => LabelsSetLookupResult::Ok(slice.into()),
        None => LabelsSetLookupResult::Err(ProfileError::NotFound),
    }
}

/// Finds the labels associated to the id.
///
/// # Safety
///
///  1. The set must be a valid reference if it's not null.
///  2. If label slices from `ddog_prof_LabelsSet_lookup` are alive, then you
///     cannot clear the set.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_LabelsSet_clear(label_set: *mut LabelsSet) {
    if let Some(label_set) = label_set.as_mut() {
        label_set.clear();
    }
}

/// Drops the set, and assigns the pointer to null to help avoid a double-free.
///
/// # Safety
///
///  1. The set must be a valid reference if it's not null.
///  2. If label slices from `ddog_prof_LabelsSet_lookup` are alive, then you
///     cannot drop the set.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_LabelsSet_drop(
    label_set: *mut *mut LabelsSet,
) {
    if let Some(ptr) = label_set.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_profiling_protobuf::StringOffset;

    #[test]
    fn test() -> Result<(), ProfileError> {
        let mut label_set = ddog_prof_LabelsSet_new();

        let labels: &[Label] = &[
            Label {
                key: StringOffset::new(1).into(),
                str: StringOffset::ZERO.into(),
                num: 13.into(),
            },
            Label {
                key: StringOffset::new(13).into(),
                str: StringOffset::ZERO.into(),
                num: 1.into(),
            },
            Label {
                key: StringOffset::new(17).into(),
                str: StringOffset::new(1).into(),
                num: 0.into(),
            },
        ];
        unsafe {
            let range = Result::from(ddog_prof_LabelsSet_insert(
                label_set,
                Slice::from(labels),
            ))?;
            let id = SliceId::from(range);
            let slice =
                Result::from(ddog_prof_LabelsSet_lookup(&mut *label_set, id))?
                    .as_slice();
            assert_eq!(slice, labels);

            ddog_prof_LabelsSet_drop(ptr::addr_of_mut!(label_set));
        }
        Ok(())
    }
}
