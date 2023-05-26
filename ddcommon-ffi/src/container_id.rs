// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::ffi::CString;
use ddcommon::container_id::get_container_id;

use std::os::raw::{c_char, c_int};

#[repr(C)]
pub struct FfiOptionString {
    pub data: *const c_char,
    pub is_some: c_int,
}

#[no_mangle]
pub extern "C" fn get_container_id_ffi(pid: c_int) -> FfiOptionString {
    let pid = if pid >= 0 { Some(pid as u32) } else { None };
    if let Some(container_id) = get_container_id(pid) {
        let container_id_cstr = CString::new(container_id).unwrap();
        FfiOptionString {
            data: container_id_cstr.into_raw(),
            is_some: 1,
        }
    } else {
        FfiOptionString {
            data: std::ptr::null(),
            is_some: 0,
        }
    }
}

#[no_mangle]
pub extern "C" fn free_container_id_ffi(data: *mut c_char) {
    if !data.is_null() {
        unsafe {
            drop(CString::from_raw(data));
        }
    }
}
