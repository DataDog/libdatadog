// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub(crate) const TRAMPOLINE_BIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin"));

pub type TrampolineFn = unsafe extern "system" fn();

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod win32;

#[cfg(windows)]
pub use crate::win32::*;
