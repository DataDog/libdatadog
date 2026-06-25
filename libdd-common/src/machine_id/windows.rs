// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HKEY};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    REG_SZ,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process};

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0u16)).collect()
}

fn is_wow64() -> bool {
    let mut result: i32 = 0;
    let ok = unsafe { IsWow64Process(GetCurrentProcess(), &mut result) };
    ok != 0 && result != 0
}

pub fn get_machine_id_impl() -> String {
    let access = if cfg!(target_pointer_width = "32") && is_wow64() {
        KEY_READ | KEY_WOW64_64KEY
    } else {
        KEY_READ
    };

    let mut hkey: HKEY = 0;
    // SAFETY: all pointers are valid.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            to_wide_null("SOFTWARE\\Microsoft\\Cryptography").as_ptr(),
            0,
            access,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS as i32 {
        return String::new();
    }

    let value_wide = to_wide_null("MachineGuid");

    let mut data_type: u32 = 0;
    let mut data_len: u32 = 0;
    // SAFETY: null data pointer is valid for a size-query call.
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            value_wide.as_ptr(),
            std::ptr::null_mut(),
            &mut data_type,
            std::ptr::null_mut(),
            &mut data_len,
        )
    };
    if status != ERROR_SUCCESS as i32 || data_type != REG_SZ {
        // SAFETY: hkey is a valid open handle.
        unsafe { RegCloseKey(hkey) };
        return String::new();
    }

    let mut buf: Vec<u16> = vec![0u16; (data_len as usize).div_ceil(2)];
    let mut actual_len = data_len;
    // SAFETY: buf has the capacity returned by the size-query call above.
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            value_wide.as_ptr(),
            std::ptr::null_mut(),
            &mut data_type,
            buf.as_mut_ptr().cast(),
            &mut actual_len,
        )
    };
    // SAFETY: hkey is a valid open handle.
    unsafe { RegCloseKey(hkey) };

    if status != ERROR_SUCCESS as i32 {
        return String::new();
    }

    while buf.last() == Some(&0u16) {
        buf.pop();
    }
    String::from_utf16(&buf)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
