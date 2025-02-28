// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(windows)]
use ddcommon_ffi::CharSlice;
#[cfg(windows)]
use libloading::{Library, Symbol};

#[cfg(not(windows))]
fn main() {
    panic!("This test is only supported on Windows");
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        panic!("Missing argument: crash path");
    }

    let crash_path = &args[1];

    let lib = unsafe { Library::new("./deps/test_app_lib.dll") }
        .expect("Failed to load test_app_lib.dll");

    unsafe {
        let func: Symbol<unsafe extern "C" fn(CharSlice) -> bool> = lib
            .get(b"init_crashtracking")
            .expect("Failed to get function from DLL");

        let result = func(CharSlice::from(crash_path.as_str()));

        if !result {
            panic!("Failed to initialize crash tracking");
        }

        println!("Crashing...");

        // Force a segfault to crash
        let ptr = std::ptr::null_mut::<i32>();
        *ptr = 42;
    }

    println!("Test app exiting (failed to crash?)");
}
