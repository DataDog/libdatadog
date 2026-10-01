// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod platform_handle;

pub mod locks;
pub mod private_dir;
pub mod process;
pub mod shm_guard;
mod shm_names;
pub(crate) use shm_names::NameOwnership;

/// Open an existing named segment exactly as its creator named it - with this platform's name
/// normalization and the filesystem fallback - refusing one another user could have created.
///
/// For readers that do their own mapping rather than going through
/// [`crate::platform::NamedShmHandle`].
pub fn open_named_shm_fd(name: &std::ffi::CStr) -> std::io::Result<std::os::fd::OwnedFd> {
    Ok(sys_open_existing(name)?)
}
pub mod sockets;
pub use shm_guard::set_shm_owner_uid;
pub use sockets::*;

mod handles;
pub use handles::*;

#[cfg(target_os = "macos")]
mod mem_handle_macos;
#[cfg(target_os = "macos")]
pub(crate) use mem_handle_macos::*;
#[cfg(not(target_os = "macos"))]
mod mem_handle;
#[cfg(not(target_os = "macos"))]
pub(crate) use mem_handle::*;

#[unsafe(no_mangle)]
#[cfg(polyfill_glibc_memfd)]
/// # Safety
/// Emulating memfd create, has the same safety level than libc::memfd_create
pub unsafe extern "C" fn memfd_create(name: libc::c_void, flags: libc::c_uint) -> libc::c_int {
    libc::syscall(libc::SYS_memfd_create, name, flags) as libc::c_int
}
