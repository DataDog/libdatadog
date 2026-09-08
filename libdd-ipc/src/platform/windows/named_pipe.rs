// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::mem;
use std::mem::MaybeUninit;
use std::os::raw::c_void;
use std::os::windows::io::RawHandle;
use std::ptr::{addr_of_mut, null_mut};
use winapi::um::winnt::WCHAR;
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows_sys::Win32::System::WindowsProgramming::{NtQueryObject, OBJECT_INFORMATION_CLASS};

#[allow(non_upper_case_globals)]
const ObjectNameInformation: OBJECT_INFORMATION_CLASS = 1i32;

#[repr(C)]
#[allow(non_snake_case)]
struct OBJECT_NAME_INFORMATION {
    Name: UNICODE_STRING,
    NameBuffer: [WCHAR; 1000],
}

pub const PIPE_PATH: &str = r"\\.\pipe\";

pub fn named_pipe_name_from_raw_handle(handle: RawHandle) -> Option<String> {
    unsafe {
        let mut ret_size: u32 = 0;
        let mut name_info = MaybeUninit::<OBJECT_NAME_INFORMATION>::uninit();
        addr_of_mut!((*name_info.as_mut_ptr()).Name).write(UNICODE_STRING {
            Length: 0,
            MaximumLength: 0,
            Buffer: null_mut(),
        });
        let mut name_info = name_info.assume_init();
        NtQueryObject(
            handle as HANDLE,
            ObjectNameInformation,
            &mut name_info as *mut OBJECT_NAME_INFORMATION as *mut c_void,
            mem::size_of::<OBJECT_NAME_INFORMATION>() as u32,
            &mut ret_size,
        );
        if name_info.Name.Buffer.is_null() {
            None
        } else {
            String::from_utf16(std::slice::from_raw_parts(
                name_info.Name.Buffer,
                (name_info.Name.Length / 2) as usize,
            ))
            .map(|path| format!("{}{}", PIPE_PATH, &path[r"\Device\NamedPipe\".len()..]))
            .ok()
        }
    }
}
