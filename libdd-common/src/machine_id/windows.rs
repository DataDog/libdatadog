// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HKEY};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    REG_SZ,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process};

/// Sub-key containing `MachineGuid`.
const SUBKEY: &str = "SOFTWARE\\Microsoft\\Cryptography";
/// Value name holding the machine GUID.
const VALUE_NAME: &str = "MachineGuid";

/// Returns `true` when the current process is a 32-bit process running under
/// WOW64 on a 64-bit Windows host.  We use this to request 64-bit registry
/// view access when reading `HKLM\SOFTWARE\Microsoft\Cryptography`, which only
/// exists in the 64-bit hive.
fn is_wow64() -> bool {
    let mut result: i32 = 0;
    let ok = unsafe { IsWow64Process(GetCurrentProcess(), &mut result) };
    ok != 0 && result != 0
}

/// Encode a Rust `&str` as a null-terminated UTF-16 (`Vec<u16>`).
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0u16)).collect()
}

/// Read the machine GUID from
/// `HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Cryptography\MachineGuid`.
///
/// Returns an empty `String` on any failure (key/value missing, access
/// denied, or encoding error), matching the Go agent's behaviour.
pub fn get_machine_id_impl() -> String {
    // On a 32-bit process running on 64-bit Windows we must request the 64-bit
    // registry view; otherwise `RegOpenKeyExW` would redirect into the WOW64
    // 32-bit hive where `MachineGuid` does not exist.
    let access = if cfg!(target_pointer_width = "32") && is_wow64() {
        KEY_READ | KEY_WOW64_64KEY
    } else {
        KEY_READ
    };

    let subkey_wide = to_wide_null(SUBKEY);

    let mut hkey: HKEY = 0;
    // SAFETY: all pointers are valid; no aliasing hazard.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey_wide.as_ptr(),
            0,
            access,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS as i32 {
        return String::new();
    }

    let result = read_string_value(hkey);

    // SAFETY: `hkey` is a valid open handle returned by `RegOpenKeyExW`.
    unsafe { RegCloseKey(hkey) };

    result
}

/// Read the `MachineGuid` REG_SZ value from an already-opened registry key
/// handle, returning an empty `String` on any failure.
fn read_string_value(hkey: HKEY) -> String {
    let value_wide = to_wide_null(VALUE_NAME);

    // First call: get the required buffer size.
    let mut data_type: u32 = 0;
    let mut data_len: u32 = 0;
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
        return String::new();
    }

    // `data_len` is in bytes (UTF-16 units × 2), including the null terminator.
    let num_u16 = (data_len as usize).div_ceil(2);
    let mut buf: Vec<u16> = vec![0u16; num_u16];

    // Second call: read the actual data.
    let mut actual_len = data_len;
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
    if status != ERROR_SUCCESS as i32 {
        return String::new();
    }

    // Strip the null terminator(s) and convert to a Rust String.
    while buf.last() == Some(&0u16) {
        buf.pop();
    }
    String::from_utf16(&buf)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
