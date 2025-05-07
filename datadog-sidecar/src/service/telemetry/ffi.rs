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

/// Enqueues a telemetry log action to be processed internally.
/// Non-blocking. Logs might be dropped if the internal queue is full.
///
/// # Safety
/// Pointers must be valid, strings must be null-terminated if not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_enqueue_telemetry_log(
    session_id_ffi: FfiString,
    runtime_id_ffi: FfiString,
    queue_id: u64,
    identifier_ffi: FfiString,
    level: CLogLevel,
    message_ffi: FfiString,
    stack_trace_ffi: Option<NonNull<FfiString>>,
    tags_ffi: Option<NonNull<FfiString>>,
    is_sensitive: bool,
) -> FfiError {
    match ddog_sidecar_enqueue_telemetry_log_result(
        session_id_ffi,
        runtime_id_ffi,
        queue_id,
        identifier_ffi,
        level,
        message_ffi,
        stack_trace_ffi,
        tags_ffi,
        is_sensitive,
    ) {
        Ok(result) => result,
        Err(e) => e,
    }
}

#[allow(clippy::too_many_arguments)]
fn ddog_sidecar_enqueue_telemetry_log_result(
    session_id_ffi: FfiString,
    runtime_id_ffi: FfiString,
    queue_id: u64,
    identifier_ffi: FfiString,
    level: CLogLevel,
    message_ffi: FfiString,
    stack_trace_ffi: Option<NonNull<FfiString>>,
    tags_ffi: Option<NonNull<FfiString>>,
    is_sensitive: bool,
) -> Result<FfiError, FfiError> {
    if session_id_ffi.is_empty()
        || runtime_id_ffi.is_empty()
        || queue_id == 0
        || identifier_ffi.is_empty()
        || message_ffi.is_empty()
    {
        return Err(FfiError::PointerNull);
    }

    let sender = match get_telemetry_action_sender() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to get telemetry action sender: {}", e);
            return Err(FfiError::OperationFailed);
        }
    };

    let instance_id = InstanceId::new(
        String::try_from(session_id_ffi)?,
        String::try_from(runtime_id_ffi)?,
    );
    let queue_id = QueueId { inner: queue_id };
    let identifier: String = identifier_ffi.try_into()?;
    let message: String = message_ffi.try_into()?;

    let stack_trace = stack_trace_ffi
        .map(|s| String::try_from(unsafe { s.as_ref() }))
        .transpose()?;
    let tags: Option<String> = tags_ffi
        .map(|s| String::try_from(unsafe { s.as_ref() }))
        .transpose()?;

    let mut hasher = DefaultHasher::new();
    identifier.hash(&mut hasher);
    let log_id = LogIdentifier {
        indentifier: hasher.finish(),
    };

    let log_data = DDLog {
        message,
        level: level.into(),
        stack_trace,
        count: 1,
        tags: tags.unwrap_or("".into()),
        is_sensitive,
        is_crash: false,
    };
    let log_action = TelemetryActions::AddLog((log_id, log_data));

    let msg = InternalTelemetryActions {
        instance_id,
        queue_id,
        actions: vec![log_action],
    };

    match sender.try_send(msg) {
        Ok(_) => Ok(FfiError::Ok),
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("Telemetry action queue full. Action dropped.");
            Err(FfiError::QueueFull)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            error!("Telemetry action receiver closed.");
            Err(FfiError::OperationFailed)
        }
    }
}
