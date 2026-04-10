// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Force page-alignment on the embedded trampoline bytes so that
// solib_bootstrap.c can mmap segments directly from /proc/self/exe.
#[repr(C, align(4096))]
struct PageAligned<T: ?Sized>(T);

static TRAMPOLINE_BIN_ALIGNED: PageAligned<
    [u8; include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin")).len()],
> = PageAligned(*include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin")));

pub(crate) const TRAMPOLINE_BIN: &[u8] = &TRAMPOLINE_BIN_ALIGNED.0;

/// C-visible pointer and length for the embedded trampoline binary.
/// Used by solib_bootstrap.c to load the trampoline ELF from memory.
///
/// Defined via global_asm! rather than #[no_mangle] to keep the symbol out of
/// Rust's auto-generated version script.  #[no_mangle] triggers
/// contains_extern_indicator() → SymbolExportLevel::C → ends up in global: of
/// the version script regardless of pub(crate).  ld 2.28 (devtoolset-7) fails
/// with "DD_TRAMPOLINE_BIN: undefined version: " when a STV_HIDDEN symbol is
/// also listed in global: of the version script.  A symbol defined purely in
/// global_asm! is invisible to rustc's export machinery, so it never enters
/// global:; it is caught by local:* and old ld handles it correctly.
///
/// .hidden + .global: STV_HIDDEN so it is not exported from the DSO, but
/// STB_GLOBAL so the linker can resolve the reference from solib_bootstrap.o
/// (a different translation unit in the same final binary).  The ptr field
/// gets R_X86_64_RELATIVE / R_AARCH64_RELATIVE because TRAMPOLINE_BIN_ALIGNED
/// is local to the DSO.
#[cfg(target_os = "linux")]
std::arch::global_asm!(
    ".pushsection .data.DD_TRAMPOLINE_BIN,\"aw\",@progbits",
    ".balign 8",
    ".hidden DD_TRAMPOLINE_BIN",
    ".global DD_TRAMPOLINE_BIN",
    ".type DD_TRAMPOLINE_BIN, @object",
    ".size DD_TRAMPOLINE_BIN, 16",
    "DD_TRAMPOLINE_BIN:",
    ".quad {data}",
    ".quad {len}",
    ".popsection",
    data = sym TRAMPOLINE_BIN_ALIGNED,
    len = const TRAMPOLINE_BIN.len(),
);

#[cfg(target_os = "windows")]
pub(crate) const CRASHTRACKING_TRAMPOLINE_BIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/crashtracking_trampoline.bin"));

pub(crate) static ENV_PASS_FD_KEY: &str = "__DD_INTERNAL_PASSED_FD";

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) const LD_PRELOAD_TRAMPOLINE_LIB: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ld_preload_trampoline.shared_lib"
));

#[cfg(target_family = "unix")]
#[macro_use]
mod unix;

#[cfg(target_family = "unix")]
pub use unix::*;

#[cfg(target_os = "windows")]
mod win32;

#[cfg(target_os = "windows")]
pub use win32::*;

use std::ffi::CString;
use std::path::PathBuf;

pub struct Entrypoint {
    pub ptr: extern "C" fn(&TrampolineData),
    pub symbol_name: CString,
}
pub enum Target {
    Entrypoint(Entrypoint),
    ManualTrampoline(String, String),
    Noop,
}

#[derive(Clone, Debug)]
pub enum LibDependency {
    Path(PathBuf),
    #[cfg(not(windows))]
    Binary(&'static [u8]),
}

impl From<Entrypoint> for Target {
    fn from(entrypoint: Entrypoint) -> Self {
        Target::Entrypoint(entrypoint)
    }
}

#[macro_export]
macro_rules! entrypoint {
    ($entrypoint:tt) => {{
        let str = concat!(stringify!($entrypoint), "\0");
        let bytes = str.as_bytes();
        $crate::Entrypoint {
            ptr: $entrypoint,
            symbol_name: unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(bytes) }.to_owned(),
        }
    }};
}

#[repr(C)]
pub struct TrampolineData {
    pub argc: i32,
    pub argv: *const *const libc::c_char,
    pub dependency_paths: *const *const libc::c_char,
}
