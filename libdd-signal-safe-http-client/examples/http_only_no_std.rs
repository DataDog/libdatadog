// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(target_os = "linux", no_std)]
#![cfg_attr(target_os = "linux", no_main)]

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
#[path = "http_only_no_std/support.rs"]
mod support;

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Origin calls this after taking over Linux process startup.
///
/// # Safety
///
/// `argc`, `argv`, and `envp` must be the initial process arguments provided by
/// the Linux program loader.
unsafe extern "C" fn origin_main(_argc: usize, _argv: *mut *mut u8, _envp: *mut *mut u8) -> i32 {
    origin::program::immediate_exit(support::run())
}

#[cfg(target_os = "linux")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    origin::program::trap()
}
