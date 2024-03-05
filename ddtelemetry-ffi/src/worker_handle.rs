// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi as ffi;
use ddtelemetry::worker::TelemetryWorkerHandle;
use ffi::slice::AsBytes;

use crate::MaybeError;

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_dependency(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    crate::try_c!(handle.add_dependency(name, version));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_integration(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
    enabled: bool,
    compatible: ffi::Option<bool>,
    auto_enabled: ffi::Option<bool>,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    crate::try_c!(handle.add_integration(
        name,
        enabled,
        version,
        compatible.into(),
        auto_enabled.into(),
    ));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_log(
    handle: &TelemetryWorkerHandle,
    indentifier: ffi::CharSlice,
    message: ffi::CharSlice,
    level: ddtelemetry::data::LogLevel,
    stack_trace: ffi::CharSlice,
) -> MaybeError {
    crate::try_c!(handle.add_log(
        indentifier.as_bytes(),
        message.to_utf8_lossy().into_owned(),
        level,
        stack_trace
            .is_empty()
            .then(|| stack_trace.to_utf8_lossy().into_owned()),
    ));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_start(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_start());
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_clone(handle: &TelemetryWorkerHandle) -> Box<TelemetryWorkerHandle> {
    Box::new(handle.clone())
}

#[no_mangle]
pub extern "C" fn ddog_handle_stop(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_stop());
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_wait_for_shutdown(handle: Box<TelemetryWorkerHandle>) {
    handle.wait_for_shutdown()
}

#[no_mangle]
pub extern "C" fn ddog_handle_drop(handle: Box<TelemetryWorkerHandle>) {
    drop(handle);
}
