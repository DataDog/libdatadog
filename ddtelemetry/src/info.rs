// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod os {
    #[cfg(unix)]
    use std::ffi::CStr;

    // TODO: this function will call API's (fargate, k8s, etc) in the future to get to real host API
    pub fn real_hostname() -> anyhow::Result<String> {
        Ok(sys_info::hostname()?)
    }

    pub const fn os_name() -> &'static str {
        std::env::consts::OS
    }

    pub fn os_version() -> anyhow::Result<String> {
        sys_info::os_release().map_err(|e| e.into())
    }

    pub fn os_type() -> Option<String> {
        sys_info::os_type().ok()
    }

    pub fn os_release() -> Option<String> {
        sys_info::os_release().ok()
    }

    /// Get string similar to `uname -a`'s output
    ///
    /// # Safety
    ///   Unsafe because of FFI, libc's uname only fails if struct utsname is
    ///   malformed, considering we `zeroed` it, it virtually cannot be
    ///   malformed. All in all pretty safe
    #[cfg(unix)]
    pub unsafe fn uname() -> Option<String> {
        let mut n = std::mem::zeroed();
        match libc::uname(&mut n) {
            0 => Some(
                CStr::from_ptr((n.version[..]).as_ptr())
                    .to_string_lossy()
                    .into_owned(),
            ),
            _ => None,
        }
    }
}
