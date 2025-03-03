// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]

use datadog_crashtracker_ffi::Metadata;
use ddcommon::Endpoint;
use ddcommon_ffi::CharSlice;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, THREAD_ERROR_MODE};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::GetCurrentProcess;

#[no_mangle]
pub extern "C" fn init_crashtracking(crash_path: CharSlice) -> bool {
    println!("init_crashtracking");

    // Make sure WER is enabled
    unsafe { SetErrorMode(THREAD_ERROR_MODE(0x0001)) };

    let process_handle = unsafe { GetCurrentProcess() };
    let module_handle = get_hmodule();

    let mut module_name_buffer = vec![0u16; 1024];

    let len = unsafe {
        GetModuleFileNameExW(
            Some(process_handle),
            Some(module_handle),
            &mut module_name_buffer,
        )
    };

    if len == 0 {
        return false;
    }

    let module_name = OsString::from_wide(&module_name_buffer[..len as usize])
        .to_string_lossy()
        .into_owned();

    println!(
        "Registering crash handler with module name: {}",
        module_name
    );

    // Check if file exists
    let path = Path::new(&module_name);
    if !path.exists() {
        println!("File does not exist: {:?}", path);
        return false;
    }

    println!("Using crash path: {}", crash_path);

    let endpoint = Endpoint::from_slice(format!("file://{}", crash_path).as_str());

    let metadata = Metadata {
        family: CharSlice::from("test_family"),
        library_name: CharSlice::from("test_library"),
        tags: None,
        library_version: CharSlice::from("test_version"),
    };

    let module_name_str = module_name;
    datadog_crashtracker_ffi::ddog_crasht_init_windows(
        CharSlice::from(module_name_str.as_str()),
        Some(&endpoint),
        metadata,
    )
}

fn get_hmodule() -> HMODULE {
    let mut module: HMODULE = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(get_hmodule as *const _),
            &mut module,
        )
        .expect("GetModuleHandleExW failed");
    }
    module
}
