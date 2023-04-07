// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_ipc::platform::PlatformHandle;
use ddcommon_ffi as ffi;
use std::{
    fs::File,
    os::unix::{net::UnixStream, prelude::FromRawFd},
};

use ddtelemetry::{
    data::{self, Dependency, Integration},
    ipc::{
        interface::{
            blocking::{self, TelemetryTransport},
            InstanceId, QueueId, RuntimeMeta,
        },
        sidecar,
    },
    worker::{LifecycleAction, TelemetryActions},
};
use ffi::slice::AsBytes;

use crate::{try_c, MaybeError};

#[repr(C)]
pub struct NativeFile {
    pub handle: Box<PlatformHandle<File>>,
}

#[repr(C)]
pub struct NativeUnixStream {
    pub handle: PlatformHandle<UnixStream>,
}

/// This creates Rust PlatformHandle<File> from supplied C std FILE object.
/// This method takes the ownership of the underlying filedescriptor.
///
/// # Safety
/// Caller must ensure the file descriptor associated with FILE pointer is open, and valid
/// Caller must not close the FILE associated filedescriptor after calling this fuction
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_ph_file_from(file: *mut libc::FILE) -> NativeFile {
    let handle = PlatformHandle::from_raw_fd(libc::fileno(file));

    NativeFile {
        handle: Box::from(handle),
    }
}

#[no_mangle]
pub extern "C" fn ddog_ph_file_clone(platform_handle: &NativeFile) -> Box<NativeFile> {
    Box::new(NativeFile {
        handle: platform_handle.handle.clone(),
    })
}

#[no_mangle]
pub extern "C" fn ddog_ph_file_drop(ph: NativeFile) {
    drop(ph)
}

#[no_mangle]
pub extern "C" fn ddog_ph_unix_stream_drop(ph: Box<NativeUnixStream>) {
    drop(ph)
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_drop(t: Box<TelemetryTransport>) {
    drop(t)
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_clone(
    transport: &TelemetryTransport,
) -> Box<TelemetryTransport> {
    Box::new(transport.clone())
}

/// # Safety
/// Caller must ensure the process is safe to fork, at the time when this method is called
#[no_mangle]
pub extern "C" fn ddog_sidecar_connect(connection: &mut *mut TelemetryTransport) -> MaybeError {
    let stream = Box::new(try_c!(sidecar::start_or_connect_to_sidecar()));
    *connection = Box::into_raw(stream);

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_ping(transport: &mut TelemetryTransport) -> MaybeError {
    try_c!(blocking::ping(transport));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_instanceId_build(
    session_id: ffi::CharSlice,
    runtime_id: ffi::CharSlice,
) -> Box<InstanceId> {
    Box::from(InstanceId::new(
        session_id.to_utf8_lossy(),
        runtime_id.to_utf8_lossy(),
    ))
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_instanceId_drop(instance_id: Box<InstanceId>) {
    drop(instance_id)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_queueId_generate() -> QueueId {
    QueueId::new_unique()
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_runtimeMeta_build(
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> Box<RuntimeMeta> {
    let inner = RuntimeMeta::new(
        language_name.to_utf8_lossy(),
        language_version.to_utf8_lossy(),
        tracer_version.to_utf8_lossy(),
    );

    Box::from(inner)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_runtimeMeta_drop(meta: Box<RuntimeMeta>) {
    drop(meta)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_enqueueConfig(
    transport: &mut Box<TelemetryTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    config_key: ffi::CharSlice,
    config_value: ffi::CharSlice,
) -> MaybeError {
    let config_entry = TelemetryActions::AddConfig(data::Configuration {
        name: config_key.to_utf8_lossy().into_owned(),
        value: config_value.to_utf8_lossy().into_owned(),
    });
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![config_entry],
    ));
    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_addDependency(
    transport: &mut Box<TelemetryTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());

    let dependency = TelemetryActions::AddDependecy(Dependency {
        name: dependency_name.to_utf8_lossy().into_owned(),
        version,
    });

    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![dependency],
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_addIntegration(
    transport: &mut Box<TelemetryTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    integration_name: ffi::CharSlice,
    integration_version: ffi::CharSlice,
) -> MaybeError {
    let version = integration_version
        .is_empty()
        .then(|| integration_version.to_utf8_lossy().into_owned());

    let integration = TelemetryActions::AddIntegration(Integration {
        name: integration_name.to_utf8_lossy().into_owned(),
        version,
        compatible: None,
        enabled: None,
        auto_enabled: None,
    });

    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![integration],
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_flushServiceData(
    transport: &mut Box<TelemetryTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    runtime_meta: &RuntimeMeta,
    service_name: ffi::CharSlice,
) -> MaybeError {
    try_c!(blocking::register_service_and_flush_queued_actions(
        transport,
        instance_id,
        queue_id,
        runtime_meta,
        service_name.to_utf8_lossy(),
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_end(
    transport: &mut Box<TelemetryTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
) -> MaybeError {
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![TelemetryActions::Lifecycle(LifecycleAction::Stop)],
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_session_config_setAgentUrl(
    transport: &mut TelemetryTransport,
    session_id: ffi::CharSlice,
    agent_url: ffi::CharSlice,
) -> MaybeError {
    try_c!(blocking::set_session_agent_url(
        transport,
        session_id.to_utf8_lossy().into(),
        agent_url.to_utf8_lossy().into()
    ));

    MaybeError::None
}
