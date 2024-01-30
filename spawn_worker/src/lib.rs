// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub(crate) const TRAMPOLINE_BIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin"));

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
    pub ptr: extern "C" fn(),
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
