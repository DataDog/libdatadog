// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! # Thread-level context sharing
//!
//! This module implements the publisher side of the Thread Context OTEP (PR #4947).
//!
//! Since `rustc` doesn't currently support the TLSDESC dialect, we use a C shim to set and get the
//! thread-local storage used for the context.

#[cfg(target_os = "linux")]
pub mod linux {
    use std::ffi::c_void;

    extern "C" {
        /// Returns the current thread's value of `custom_labels_current_set_v2`.
        fn libdd_get_custom_labels_current_set_v2() -> *mut *mut c_void;
    }

    /// Read the TLS pointer for the current thread.
    #[allow(clippy::missing_safety_doc)]
    pub(crate) unsafe fn get_tls_ptr() -> *mut *mut c_void {
        libdd_get_custom_labels_current_set_v2()
    }
}
