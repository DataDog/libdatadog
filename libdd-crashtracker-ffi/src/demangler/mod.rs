// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
mod datatypes;
pub use datatypes::*;

use ::function_name::named;
use libdd_common_ffi::{slice::AsBytes, wrap_with_ffi_result, CharSlice, StringWrapperResult};
use symbolic_common::Name;
use symbolic_demangle::Demangle;

/// Demangles the string "name".
/// If demangling fails, returns an empty string ""
///
/// # Safety
/// `name` should be a valid reference to a utf8 encoded String.
/// The string is copied into the result, and does not need to outlive this call
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_demangle(
    name: CharSlice,
    options: DemangleOptions,
) -> StringWrapperResult {
    wrap_with_ffi_result!({
        let name = name.to_utf8_lossy();
        let name = Name::from(name);
        let options = match options {
            DemangleOptions::Complete => symbolic_demangle::DemangleOptions::complete(),
            DemangleOptions::NameOnly => symbolic_demangle::DemangleOptions::name_only(),
        };
        anyhow::Ok(name.demangle(options).unwrap_or_default())
    })
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_demangle() {
    // It appears that Miri might change the behavior of symbolic_common::Name::demangle, so we
    // don't run this test under Miri.
    let test_string = "_ZNSt28__atomic_futex_unsigned_base26_M_futex_wait_until_steadyEPjjbNSt6chrono8durationIlSt5ratioILl1ELl1EEEENS2_IlS3_ILl1ELl1000000000EEEE";
    let test_slice = CharSlice::from(test_string);
    let result: String = unsafe { ddog_crasht_demangle(test_slice, DemangleOptions::Complete) }
        .unwrap()
        .into();
    assert_eq!(result, "std::__atomic_futex_unsigned_base::_M_futex_wait_until_steady(unsigned int*, unsigned int, bool, std::chrono::duration<long, std::ratio<(long)1, (long)1> >, std::chrono::duration<long, std::ratio<(long)1, (long)1000000000> >)");

    let result: String = unsafe { ddog_crasht_demangle(test_slice, DemangleOptions::NameOnly) }
        .unwrap()
        .into();
    assert_eq!(
        result,
        "std::__atomic_futex_unsigned_base::_M_futex_wait_until_steady"
    );
}

#[test]
fn test_demangle_fails() {
    let test_string = "_ZNSt28__fdf";
    let test_slice = CharSlice::from(test_string);
    let result: String = unsafe { ddog_crasht_demangle(test_slice, DemangleOptions::Complete) }
        .unwrap()
        .into();
    assert_eq!(result, "");

    let result: String = unsafe { ddog_crasht_demangle(test_slice, DemangleOptions::NameOnly) }
        .unwrap()
        .into();
    assert_eq!(result, "");
}
