// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//use std::os::raw::c_void;
use std::ffi::c_void;
use ddcommon::{TracerMetadata, AnonymousFileHandle, store_tracer_metadata};

impl AnonymousFileHandle {
    fn as_raw_pointer(&self) -> *mut c_void {
        match self {
            #[cfg(target_os = "linux")]
            AnonymousFileHandle::Linux(memfd) => Box::as_ptr(memfd) as *mut c_void,
            #[cfg(not(target_os = "linux"))]
            AnonymousFileHandle::Other(()) => std::ptr::null() as *mut c_void,
        }
    }
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_store_tracer_metadata(tracer_metadata: TracerMetadata) -> *mut c_void {
    let handle = store_tracer_metadata(tracer_metadata).unwrap();
    return handle.as_raw_pointer();
}
