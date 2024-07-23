// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Represents a CrashInfo. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct CrashInfo {
    // This may be null, but if not it will point to a valid CrashInfo.
    inner: *mut datadog_crashtracker::CrashInfo,
}

impl CrashInfo {
    pub(super) fn new(crash_info: datadog_crashtracker::CrashInfo) -> Self {
        CrashInfo {
            inner: Box::into_raw(Box::new(crash_info)),
        }
    }

    pub(super) fn take(&mut self) -> Option<Box<datadog_crashtracker::CrashInfo>> {
        // Leaving a null will help with double-free issues that can
        // arise in C. Of course, it's best to never get there in the
        // first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for CrashInfo {
    fn drop(&mut self) {
        drop(self.take())
    }
}

pub(crate) unsafe fn crashinfo_ptr_to_inner<'a>(
    crashinfo_ptr: *mut CrashInfo,
) -> anyhow::Result<&'a mut datadog_crashtracker::CrashInfo> {
    match crashinfo_ptr.as_mut() {
        None => anyhow::bail!("crashinfo pointer was null"),
        Some(inner_ptr) => match inner_ptr.inner.as_mut() {
            Some(crashinfo) => Ok(crashinfo),
            None => anyhow::bail!("crashinfo's inner pointer was null (indicates use-after-free)"),
        },
    }
}

/// Returned by [ddog_prof_Profile_new].
#[repr(C)]
pub enum CrashInfoNewResult {
    Ok(CrashInfo),
    #[allow(dead_code)]
    Err(Error),
}
