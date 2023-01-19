// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon_ffi as ffi;
use std::{
    fs::File,
    os::unix::{net::UnixStream, prelude::FromRawFd},
};

use ddtelemetry::{
    data::{Dependency, DependencyType, Integration},
    ipc::{
        interface::{
            blocking::{self, TelemetryTransport},
            InstanceId, QueueId, RuntimeMeta,
        },
        platform::PlatformHandle,
        sidecar,
    },
    worker::TelemetryActions, mock_telemetry_target::{self, MockServer},
};
use ffi::slice::AsBytes;

use crate::{try_c, MaybeError};

#[repr(C)]
pub struct NativeFile {
    handle: Box<PlatformHandle<File>>
}

#[repr(C)]
pub struct NativeUnixStream {
    handle: PlatformHandle<UnixStream>
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

    NativeFile { handle: Box::from( handle) }
}

#[no_mangle]
pub extern "C" fn ddog_ph_file_clone(
    platform_handle: &NativeFile,
) -> Box<NativeFile> {
    Box::new(NativeFile { handle: platform_handle.handle.clone() })
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
pub extern "C" fn ddog_sidecar_ping(transport: &mut Box<TelemetryTransport>) -> MaybeError {
    try_c!(blocking::ping(transport));

    MaybeError::None
}

#[no_mangle]
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
pub unsafe extern "C" fn ddog_sidecar_instanceId_drop(instance_id: Box<InstanceId>) {
    drop(instance_id)
}

#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_queueId_generate() -> QueueId {
    QueueId::new_unique()
}

#[no_mangle]
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
pub unsafe extern "C" fn ddog_sidecar_runtimeMeta_drop(meta: Box<RuntimeMeta>) {
    drop(meta)
}

#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_telemetry_enqueueConfig(
    transport: &mut Box<TelemetryTransport>,
    instance_id: Box<InstanceId>,
    queue_id: &QueueId,
    config_key: ffi::CharSlice,
    config_value: ffi::CharSlice,
) -> MaybeError {
    let config_entry = TelemetryActions::AddConfig((
        config_key.to_utf8_lossy().into_owned(),
        config_value.to_utf8_lossy().into_owned(),
    ));
    try_c!(blocking::enqueue_actions(
        transport,
        &instance_id,
        queue_id,
        vec![config_entry],
    ));
    MaybeError::None
}

#[no_mangle]
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
        hash: None,
        type_: DependencyType::PlatformStandard,
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
        &service_name.to_utf8_lossy().into(),
    ));

    MaybeError::None
}

#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_mock_start(result: &mut *mut MockServer) -> MaybeError {
    let server = try_c!(mock_telemetry_target::MockServer::start_random_local_port());

    *result = Box::into_raw(Box::new(server));
    MaybeError::None
}

#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_session_config_setAgentUrl(transport: &mut Box<TelemetryTransport>, session_id: ffi::CharSlice,
    agent_url: ffi::CharSlice) -> MaybeError {
    try_c!(blocking::set_session_agent_url(transport, session_id.to_utf8_lossy().into(), agent_url.to_utf8_lossy().into()));

    MaybeError::None
}

#[cfg(test)]
mod test_c_sidecar {

    use super::*;
    use std::{ffi::CString, io::Write, os::unix::prelude::AsRawFd};

    #[test]
    fn test_ddog_ph_file_handling() {
        let fname = CString::new(std::env::temp_dir().join("test_file").to_str().unwrap()).unwrap();
        let mode = CString::new("a+").unwrap();

        let file = unsafe { libc::fopen(fname.as_ptr(), mode.as_ptr()) };
        let file = unsafe { ddog_ph_file_from(file) };
        let fd = file.handle.as_raw_fd();
        {
            let mut file = &*file.handle.as_filelike_view().unwrap();
            writeln!(file, "test").unwrap();
        }
        ddog_ph_file_drop(file);

        let mut file = unsafe { File::from_raw_fd(fd) };
        writeln!(file, "test").unwrap_err(); // file is closed, so write returns an error
    }

    #[test]
    #[ignore] // run all tests that can fork in a separate run, to avoid any race conditions with default rust test harness
    fn test_ddog_sidecar_connection() {
        let mut transport = std::ptr::null_mut();
        assert_eq!(ddog_sidecar_connect(&mut transport), MaybeError::None);
        let mut transport = unsafe { Box::from_raw(transport) };
        assert_eq!(ddog_sidecar_ping(&mut transport), MaybeError::None);

        ddog_sidecar_transport_drop(transport);
    }

    #[test]
    #[ignore] // run all tests that can fork in a separate run, to avoid any race conditions with default rust test harness
    fn test_ddog_sidecar_register_app() {
        let mut transport = std::ptr::null_mut();
        assert_eq!(ddog_sidecar_connect(&mut transport), MaybeError::None);
        let mut transport = unsafe { Box::from_raw(transport) };
        unsafe {
            ddog_sidecar_session_config_setAgentUrl(&mut transport, "session_id".into(), "http://localhost:8082/".into());

            let meta = ddog_sidecar_runtimeMeta_build(
                "language_name".into(),
                "language_version".into(),
                "tracer_version".into(),
            );

            let instance_id =
                ddog_sidecar_instanceId_build("session_id".into(), "runtime_id".into());
            let queue_id = ddog_sidecar_queueId_generate();

            ddog_sidecar_telemetry_addDependency(
                &mut transport,
                &instance_id,
                &queue_id,
                "dependency_name".into(),
                "dependency_version".into(),
            );

            // ddog_sidecar_telemetry_addIntegration(&mut transport, instance_id, &queue_id, integration_name, integration_version)
            // TODO add ability to add configuration

            assert_eq!(
                ddog_sidecar_telemetry_flushServiceData(
                    &mut transport,
                    &instance_id,
                    &queue_id,
                    &meta,
                    "service_name".into()
                ),
                MaybeError::None
            );
            // reset session config - and cause shutdown of all existing instances
            ddog_sidecar_session_config_setAgentUrl(&mut transport, "session_id".into(), "".into());

            //TODO: Shutdown the service
            // enough case: have C api that shutsdown telemetry worker
            // ideal case : when connection socket is closed by the client the telemetry worker shuts down automatically 
            ddog_sidecar_instanceId_drop(instance_id);
            ddog_sidecar_runtimeMeta_drop(meta);
        };

        ddog_sidecar_transport_drop(transport);
    }
}
