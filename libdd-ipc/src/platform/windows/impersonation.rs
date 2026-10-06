// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::ptr::null;
use tracing::{error, warn};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NO_TOKEN, HANDLE};
use windows_sys::Win32::Security::{RevertToSelf, TOKEN_IMPERSONATE};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken, SetThreadToken};

/// Suspends the calling thread's impersonation until dropped, so that OS calls are access-checked
/// against the process identity, which owns the sidecar and its pipes and shared memory.
pub struct ProcessIdentityGuard(HANDLE);

impl ProcessIdentityGuard {
    /// A no-op guard if the thread is not impersonating, or if impersonation cannot be suspended.
    #[must_use]
    pub fn enter() -> Self {
        let mut token: HANDLE = 0;
        // OpenAsSelf: the impersonated user may not be allowed to open its own thread token.
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_IMPERSONATE, 1, &mut token) } == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                warn!("Failed opening the thread token, keeping impersonation: {err}");
            }
            return Self(0);
        }
        if unsafe { RevertToSelf() } == 0 {
            warn!(
                "Failed suspending impersonation: {}",
                io::Error::last_os_error()
            );
            unsafe { CloseHandle(token) };
            return Self(0);
        }
        Self(token)
    }
}

impl Drop for ProcessIdentityGuard {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        if unsafe { SetThreadToken(null(), self.0) } == 0 {
            // Returning to the caller with the process identity instead of its own is not safe.
            error!(
                "Failed restoring impersonation: {}",
                io::Error::last_os_error()
            );
            std::process::abort();
        }
        unsafe { CloseHandle(self.0) };
    }
}
