// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_ipc::platform::{
    FileBackedHandle, MappedMem, NamedShmHandle, PlatformHandle, ShmHandle,
};
use datadog_sidecar::agent_remote_config::{
    new_reader, reader_from_shm, AgentRemoteConfigEndpoint, AgentRemoteConfigWriter,
};
use datadog_sidecar::config;
use datadog_sidecar::config::LogMethod;
use ddcommon_ffi as ffi;
use libc::c_char;
use std::ffi::c_void;
use std::time::Duration;
use std::{fs::File, os::unix::prelude::FromRawFd, slice};

use datadog_sidecar::interface::{
    blocking::{self, SidecarTransport},
    InstanceId, QueueId, RuntimeMeta, SerializedTracerHeaderTags, SessionConfig,
};
use datadog_sidecar::one_way_shared_memory::{OneWayShmReader, ReaderOpener};
use ddcommon::Endpoint;
use ddtelemetry::{
    data::{self, Dependency, Integration},
    worker::{LifecycleAction, TelemetryActions},
};
use ffi::slice::AsBytes;

use ddtelemetry_ffi::{try_c, MaybeError};

#[repr(C)]
pub struct NativeFile {
    pub handle: Box<PlatformHandle<File>>,
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

pub enum AgentRemoteConfigReader {
    Named(datadog_sidecar::agent_remote_config::AgentRemoteConfigReader<NamedShmHandle>),
    Unnamed(datadog_sidecar::agent_remote_config::AgentRemoteConfigReader<ShmHandle>),
}

#[no_mangle]
pub extern "C" fn ddog_alloc_anon_shm_handle(
    size: usize,
    handle: &mut *mut ShmHandle,
) -> MaybeError {
    *handle = Box::into_raw(Box::new(try_c!(ShmHandle::new(size))));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_map_shm(
    handle: Box<ShmHandle>,
    mapped: &mut *mut MappedMem<ShmHandle>,
    pointer: &mut *mut c_void,
    size: &mut usize,
) -> MaybeError {
    let mut memory_mapped = try_c!(handle.map());
    let slice = memory_mapped.as_slice_mut();
    *pointer = slice as *mut [u8] as *mut c_void;
    *size = slice.len();

    *mapped = Box::into_raw(Box::new(memory_mapped));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_unmap_shm(mapped: Box<MappedMem<ShmHandle>>) -> Box<ShmHandle> {
    Box::new((*mapped).into())
}

#[no_mangle]
pub extern "C" fn ddog_drop_anon_shm_handle(_: Box<ShmHandle>) {}

#[no_mangle]
pub extern "C" fn ddog_create_agent_remote_config_writer(
    writer: &mut *mut AgentRemoteConfigWriter<ShmHandle>,
    handle: &mut *mut ShmHandle,
) -> MaybeError {
    let (new_writer, new_handle) = try_c!(datadog_sidecar::agent_remote_config::create_anon_pair());
    *writer = Box::into_raw(Box::new(new_writer));
    *handle = Box::into_raw(Box::new(new_handle));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_agent_remote_config_reader_for_endpoint(
    endpoint: &Endpoint,
) -> Box<AgentRemoteConfigReader> {
    Box::new(AgentRemoteConfigReader::Named(new_reader(endpoint)))
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_agent_remote_config_reader_for_anon_shm(
    handle: &ShmHandle,
    reader: &mut *mut AgentRemoteConfigReader,
) -> MaybeError {
    *reader = Box::into_raw(Box::new(AgentRemoteConfigReader::Unnamed(try_c!(
        reader_from_shm(handle.clone())
    ))));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_agent_remote_config_write(
    writer: &AgentRemoteConfigWriter<ShmHandle>,
    data: ffi::CharSlice,
) {
    writer.write(data.as_bytes());
}

fn ddog_agent_remote_config_read_generic<'a, T>(
    reader: &'a mut datadog_sidecar::agent_remote_config::AgentRemoteConfigReader<T>,
    data: &mut ffi::CharSlice<'a>,
) -> bool
where
    T: FileBackedHandle + From<MappedMem<T>>,
    OneWayShmReader<T, Option<AgentRemoteConfigEndpoint>>: ReaderOpener<T>,
{
    let (new, contents) = reader.read();
    // c_char may be u8 or i8 depending on target... convert it.
    let contents: &[c_char] = unsafe { std::mem::transmute::<&[u8], &[c_char]>(contents) };
    *data = contents.into();
    new
}

#[no_mangle]
pub extern "C" fn ddog_agent_remote_config_read<'a>(
    reader: &'a mut AgentRemoteConfigReader,
    data: &mut ffi::CharSlice<'a>,
) -> bool {
    match reader {
        AgentRemoteConfigReader::Named(reader) => {
            ddog_agent_remote_config_read_generic(reader, data)
        }
        AgentRemoteConfigReader::Unnamed(reader) => {
            ddog_agent_remote_config_read_generic(reader, data)
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_agent_remote_config_reader_drop(_: Box<AgentRemoteConfigReader>) {}

#[no_mangle]
pub extern "C" fn ddog_agent_remote_config_writer_drop(_: Box<AgentRemoteConfigWriter<ShmHandle>>) {
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_drop(_: Box<SidecarTransport>) {}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_clone(
    transport: &SidecarTransport,
) -> Box<SidecarTransport> {
    Box::new(transport.clone())
}

/// # Safety
/// Caller must ensure the process is safe to fork, at the time when this method is called
#[no_mangle]
pub extern "C" fn ddog_sidecar_connect(connection: &mut *mut SidecarTransport) -> MaybeError {
    let cfg = datadog_sidecar::config::Config::get();

    let stream = Box::new(try_c!(datadog_sidecar::start_or_connect_to_sidecar(cfg)));
    *connection = Box::into_raw(stream);

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_ping(transport: &mut Box<SidecarTransport>) -> MaybeError {
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
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    config_key: ffi::CharSlice,
    config_value: ffi::CharSlice,
    origin: data::ConfigurationOrigin,
) -> MaybeError {
    let config_entry = TelemetryActions::AddConfig(data::Configuration {
        name: config_key.to_utf8_lossy().into_owned(),
        value: config_value.to_utf8_lossy().into_owned(),
        origin,
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
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let version =
        (!dependency_version.is_empty()).then(|| dependency_version.to_utf8_lossy().into_owned());

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
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    integration_name: ffi::CharSlice,
    integration_version: ffi::CharSlice,
    integration_enabled: bool,
) -> MaybeError {
    let version =
        (!integration_version.is_empty()).then(|| integration_version.to_utf8_lossy().into_owned());

    let integration = TelemetryActions::AddIntegration(Integration {
        name: integration_name.to_utf8_lossy().into_owned(),
        enabled: integration_enabled,
        version,
        compatible: None,
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
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    runtime_meta: &RuntimeMeta,
    service_name: ffi::CharSlice,
    env_name: ffi::CharSlice,
) -> MaybeError {
    try_c!(blocking::register_service_and_flush_queued_actions(
        transport,
        instance_id,
        queue_id,
        runtime_meta,
        service_name.to_utf8_lossy(),
        env_name.to_utf8_lossy(),
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_end(
    transport: &mut Box<SidecarTransport>,
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
pub extern "C" fn ddog_sidecar_is_closed(transport: &mut Box<SidecarTransport>) -> bool {
    transport.is_closed()
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_session_set_config(
    transport: &mut Box<SidecarTransport>,
    session_id: ffi::CharSlice,
    endpoint: &Endpoint,
    flush_interval_milliseconds: u64,
    force_flush_size: usize,
    force_drop_size: usize,
    log_level: ffi::CharSlice,
    log_path: ffi::CharSlice,
) -> MaybeError {
    try_c!(blocking::set_session_config(
        transport,
        session_id.to_utf8_lossy().into(),
        &SessionConfig {
            endpoint: endpoint.clone(),
            flush_interval: Duration::from_millis(flush_interval_milliseconds),
            force_flush_size,
            force_drop_size,
            log_level: log_level.to_utf8_lossy().into(),
            log_file: if log_path.is_empty() {
                config::FromEnv::log_method()
            } else {
                LogMethod::File(String::from(log_path.to_utf8_lossy()).into())
            }
        },
    ));

    MaybeError::None
}

#[repr(C)]
pub struct TracerHeaderTags<'a> {
    pub lang: ffi::CharSlice<'a>,
    pub lang_version: ffi::CharSlice<'a>,
    pub lang_interpreter: ffi::CharSlice<'a>,
    pub lang_vendor: ffi::CharSlice<'a>,
    pub tracer_version: ffi::CharSlice<'a>,
    pub container_id: ffi::CharSlice<'a>,
    pub client_computed_top_level: bool,
    pub client_computed_stats: bool,
}

impl<'a> From<&'a TracerHeaderTags<'a>> for SerializedTracerHeaderTags {
    fn from(tags: &'a TracerHeaderTags<'a>) -> Self {
        datadog_trace_utils::trace_utils::TracerHeaderTags {
            lang: &tags.lang.to_utf8_lossy(),
            lang_version: &tags.lang_version.to_utf8_lossy(),
            lang_interpreter: &tags.lang_interpreter.to_utf8_lossy(),
            lang_vendor: &tags.lang_vendor.to_utf8_lossy(),
            tracer_version: &tags.tracer_version.to_utf8_lossy(),
            container_id: &tags.container_id.to_utf8_lossy(),
            client_computed_top_level: tags.client_computed_top_level,
            client_computed_stats: tags.client_computed_stats,
        }
        .into()
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_send_trace_v04_shm(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    shm_handle: Box<ShmHandle>,
    tracer_header_tags: &TracerHeaderTags,
) -> MaybeError {
    try_c!(blocking::send_trace_v04_shm(
        transport,
        instance_id,
        *shm_handle,
        tracer_header_tags.into(),
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_send_trace_v04_bytes(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    data: ffi::CharSlice,
    tracer_header_tags: &TracerHeaderTags,
) -> MaybeError {
    try_c!(blocking::send_trace_v04_bytes(
        transport,
        instance_id,
        data.as_bytes().to_vec(),
        tracer_header_tags.into(),
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dump(
    transport: &mut Box<SidecarTransport>,
) -> ffi::CharSlice {
    let str = match blocking::dump(transport) {
        Ok(dump) => dump,
        Err(e) => format!("{:?}", e),
    };
    let size = str.len();
    let malloced = libc::malloc(size) as *mut u8;
    let buf = slice::from_raw_parts_mut(malloced, size);
    buf.copy_from_slice(str.as_bytes());
    ffi::CharSlice::from_raw_parts(malloced as *mut c_char, size)
}
