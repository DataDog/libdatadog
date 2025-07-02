// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::missing_safety_doc)]

use crate::service::{InstanceId, QueueId};
use ddtelemetry::data::{Log as DDLog, LogLevel as DDLogLevel};
use ddtelemetry::worker::{LogIdentifier, TelemetryActions};
use std::collections::hash_map::DefaultHasher;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;
use tokio::sync::mpsc;
use tracing::{error, warn};

use super::{get_telemetry_action_sender, InternalTelemetryActions};

#[repr(C)]
pub enum FfiError {
    Ok = 0,
    PointerNull = 1,
    OperationFailed = 2,
    InvalidUtf8 = 3,
    QueueFull = 4,
}

#[repr(C)]
pub enum CLogLevel {
    Error = 1,
    Warn = 2,
    Debug = 3,
}

impl From<CLogLevel> for DDLogLevel {
    fn from(level: CLogLevel) -> Self {
        match level {
            CLogLevel::Error => DDLogLevel::Error,
            CLogLevel::Warn => DDLogLevel::Warn,
            CLogLevel::Debug => DDLogLevel::Debug,
        }
    }
}

#[repr(C)]
pub struct FfiString {
    ptr: *const libc::c_char,
    len: usize,
}

impl FfiString {
    fn is_empty(&self) -> bool {
        self.len == 0 || self.ptr.is_null()
    }
}

impl TryFrom<&FfiString> for String {
    type Error = FfiError;

    fn try_from(vref: &FfiString) -> Result<Self, Self::Error> {
        if vref.ptr.is_null() {
            return Err(FfiError::PointerNull);
        }
        let cstr = unsafe {
            CStr::from_ptr(vref.ptr)
                .to_str()
                .map_err(|_| FfiError::InvalidUtf8)
        }?;
        Ok(cstr.to_string())
    }
}

impl TryFrom<FfiString> for String {
    type Error = FfiError;

    fn try_from(value: FfiString) -> Result<Self, Self::Error> {
        String::try_from(&value)
    }
}

