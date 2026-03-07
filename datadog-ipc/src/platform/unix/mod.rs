// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod platform_handle;

pub mod locks;
pub mod sockets;
pub use sockets::*;

mod message;
pub use message::*;

#[cfg(target_os = "macos")]
mod mem_handle_macos;
#[cfg(target_os = "macos")]
pub(crate) use mem_handle_macos::*;
#[cfg(not(target_os = "macos"))]
mod mem_handle;
#[cfg(not(target_os = "macos"))]
pub(crate) use mem_handle::*;

#[no_mangle]
#[cfg(polyfill_glibc_memfd)]
/// # Safety
/// Emulating memfd create, has the same safety level than libc::memfd_create
pub unsafe extern "C" fn memfd_create(name: libc::c_void, flags: libc::c_uint) -> libc::c_int {
    libc::syscall(libc::SYS_memfd_create, name, flags) as libc::c_int
}

