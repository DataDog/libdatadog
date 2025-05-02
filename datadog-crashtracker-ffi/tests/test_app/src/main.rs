// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(windows)]
use std::hint::black_box;

#[cfg(not(windows))]
fn main() {
    panic!("This test is only supported on Windows");
}

#[cfg(windows)]
fn main() {
    // Usage: test_app.exe <path to write crash report>
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        panic!("Missing argument: crash path");
    }

    let crash_path = &args[1];
    println!("Crash path: {}", crash_path);

    // Get the directory of the current exe
    let exe_path = std::env::current_exe().unwrap();
    let exe_dir = exe_path.parent().unwrap();
    let lib_path = exe_dir.join("datadog_crashtracker_ffi.dll");

    init_crashtracking(crash_path, lib_path.to_str().unwrap());

    // Force a segfault to crash
    let ptr = std::ptr::null_mut::<i32>();
    // SAFETY: Don't worry, we are crashing on purpose
    unsafe { *black_box(ptr) = black_box(42) };
    println!("Test app exiting (failed to crash?)");
}

#[cfg(windows)]
fn init_crashtracking(crash_path: &str, module_name: &str) -> bool {
    use datadog_crashtracker_ffi::Metadata;
    use ddcommon::Endpoint;
    use ddcommon_ffi::CharSlice;
    use std::path::Path;
    use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, THREAD_ERROR_MODE};

    // Make sure WER is enabled
    // SAFETY: No preconditions
    unsafe { SetErrorMode(THREAD_ERROR_MODE(0x0001)) };

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

    let endpoint = Endpoint::from_slice(format!("file://{}", crash_path).as_str());

    let metadata = Metadata {
        family: CharSlice::from("test_family"),
        library_name: CharSlice::from("test_library"),
        tags: None,
        library_version: CharSlice::from("test_version"),
    };

    let module_name_str = module_name;
    datadog_crashtracker_ffi::ddog_crasht_init_windows(
        CharSlice::from(module_name_str),
        Some(&endpoint),
        metadata,
    )
}
