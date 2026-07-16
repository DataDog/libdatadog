// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows host machine id: reads `MachineGuid` from
//! `HKLM\SOFTWARE\Microsoft\Cryptography` via the raw Win32 registry API.
//!
//! `MachineGuid` lives in the 64-bit registry view, so a 32-bit process under
//! WOW64 must pass `KEY_WOW64_64KEY` or it gets redirected to the (empty)
//! `WOW6432Node` copy.
//! The value is read with two calls in Win32:
//! query the byte size, then read into a right-sized buffer.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_WOW64_64KEY, REG_SZ,
};

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0u16)).collect()
}

pub fn get_machine_id_impl() -> String {
    // MachineGuid is in the 64-bit view; a 32-bit process under WOW64 is
    // redirected to WOW6432Node by default, so force the 64-bit view there.
    let access = KEY_READ | KEY_WOW64_64KEY;

    let mut hkey: HKEY = 0;
    // SAFETY: all pointers are valid.
    let subkey = to_wide_null("SOFTWARE\\Microsoft\\Cryptography");
    let status =
        unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, access, &mut hkey) };
    if status != ERROR_SUCCESS {
        return String::new();
    }

    let value_wide = to_wide_null("MachineGuid");

    let mut data_type: u32 = 0;
    let mut data_len: u32 = 0;
    // SAFETY: null data pointer is valid for a size-query call.
    // First call (null data pointer) returns the value's size in bytes.
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            value_wide.as_ptr(),
            core::ptr::null_mut(),
            &mut data_type,
            core::ptr::null_mut(),
            &mut data_len,
        )
    };
    if status != ERROR_SUCCESS || data_type != REG_SZ {
        // SAFETY: hkey is a valid open handle.
        unsafe { RegCloseKey(hkey) };
        return String::new();
    }

    // data_len is bytes; REG_SZ holds UTF-16, so the u16 count is bytes/2 (round up).
    let mut buf: Vec<u16> = vec![0u16; (data_len as usize).div_ceil(2)];
    let mut actual_len = data_len;
    // SAFETY: buf has the capacity returned by the size-query call above.
    // Second call reads the value into the size-query'd buffer.
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            value_wide.as_ptr(),
            core::ptr::null_mut(),
            &mut data_type,
            buf.as_mut_ptr().cast(),
            &mut actual_len,
        )
    };
    // SAFETY: hkey is a valid open handle.
    unsafe { RegCloseKey(hkey) };

    if status != ERROR_SUCCESS {
        return String::new();
    }

    // REG_SZ data includes a trailing NUL (possibly padded with more); drop them.
    while buf.last() == Some(&0u16) {
        buf.pop();
    }
    String::from_utf16(&buf)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
