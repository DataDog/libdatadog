// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data;

pub fn build_host() -> data::Host {
    tracing::debug!("Building telemetry host information");
    let hostname = os::real_hostname().unwrap_or_else(|_| String::from("unknown_hostname"));
    let container_id = libdd_common::entity_id::get_container_id().map(|f| f.to_string());
    let os_version = os::os_version().ok();

    tracing::debug!(
        host.hostname = %hostname,
        host.container_id = ?container_id,
        host.os = os::os_name(),
        host.os_version = ?os_version,
        host.architecture = os::architecture(),
        "Built telemetry host information"
    );

    data::Host {
        hostname,
        container_id,
        os: Some(String::from(os::os_name())),
        os_version,
        architecture: Some(String::from(os::architecture())),
        kernel_name: os::os_type(),
        kernel_release: os::os_release(),
        #[cfg(unix)]
        kernel_version: unsafe { os::uname() },
        #[cfg(windows)]
        kernel_version: winver::WindowsVersion::detect()
            .map(|wv| format!("{}.{}.{}", wv.major, wv.minor, wv.build)),
        #[cfg(not(any(windows, unix)))]
        kernel_version: None,
    }
}

pub mod os {
    #[cfg(unix)]
    use std::ffi::CStr;

    // TODO: this function will call API's (fargate, k8s, etc) in the future to get to real host API
    #[cfg(not(target_arch = "wasm32"))]
    pub fn real_hostname() -> anyhow::Result<String> {
        Ok(sys_info::hostname()?)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn real_hostname() -> anyhow::Result<String> {
        anyhow::bail!("hostname not available on wasm")
    }

    pub const fn os_name() -> &'static str {
        std::env::consts::OS
    }

    pub const fn architecture() -> &'static str {
        std::env::consts::ARCH
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn os_version() -> anyhow::Result<String> {
        sys_info::os_release().map_err(|e| e.into())
    }

    #[cfg(target_arch = "wasm32")]
    pub fn os_version() -> anyhow::Result<String> {
        anyhow::bail!("os_version not available on wasm")
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn os_type() -> Option<String> {
        sys_info::os_type().ok()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn os_type() -> Option<String> {
        None
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn os_release() -> Option<String> {
        sys_info::os_release().ok()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn os_release() -> Option<String> {
        None
    }

    /// Get string similar to `uname -a`'s output
    ///
    /// # Safety
    ///   Unsafe because of FFI, libc's uname only fails if struct utsname is
    ///   malformed, considering we `zeroed` it, it virtually cannot be
    ///   malformed. All in all pretty safe
    #[cfg(unix)]
    pub unsafe fn uname() -> Option<String> {
        unsafe {
            let mut n = std::mem::zeroed();
            match libc::uname(&mut n) {
                0 => Some(
                    CStr::from_ptr(n.version.as_ptr())
                        .to_string_lossy()
                        .into_owned(),
                ),
                _ => None,
            }
        }
    }
}
