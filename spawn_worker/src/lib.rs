// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub(crate) const TRAMPOLINE_BIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin"));

#[cfg(target_family = "unix")]
mod unix;
#[cfg(target_family = "unix")]
pub use unix::*;
