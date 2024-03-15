// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod stats_exporter;
mod trace_exporter;

#[macro_export]
macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return ddcommon_ffi::Option::Some(ddcommon_ffi::Error::from(e)),
        }
    };
}
