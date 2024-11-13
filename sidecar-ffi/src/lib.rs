// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ipc::platform::{
    FileBackedHandle, MappedMem, NamedShmHandle, PlatformHandle, ShmHandle,
};
use datadog_live_debugger::debugger_defs::DebuggerPayload;
use datadog_remote_config::fetch::ConfigInvariants;
use datadog_remote_config::{RemoteConfigCapabilities, RemoteConfigProduct, Target};
use datadog_sidecar::agent_remote_config::{
    new_reader, reader_from_shm, AgentRemoteConfigEndpoint, AgentRemoteConfigWriter,
};
use datadog_sidecar::config;
use datadog_sidecar::config::LogMethod;
use datadog_sidecar::crashtracker::crashtracker_unix_socket_path;
use datadog_sidecar::one_way_shared_memory::{OneWayShmReader, ReaderOpener};
use datadog_sidecar::service::agent_info::AgentInfoReader;
use datadog_sidecar::service::{
    blocking::{self, SidecarTransport},
    InstanceId, QueueId, RuntimeMetadata, SerializedTracerHeaderTags, SessionConfig, SidecarAction,
};
use datadog_sidecar::shm_remote_config::{path_for_remote_config, RemoteConfigReader};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use ddcommon_ffi as ffi;
use ddcommon_ffi::{CharSlice, MaybeError};
use ddtelemetry::{
    data::{self, Dependency, Integration},
    worker::{LifecycleAction, TelemetryActions},
};
use ddtelemetry_ffi::try_c;
use dogstatsd_client::DogStatsDActionOwned;
use ffi::slice::AsBytes;
use libc::c_char;
use std::ffi::{c_void, CStr, CString};
use std::fs::File;
#[cfg(unix)]
use std::os::unix::prelude::FromRawFd;
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::slice;
use std::sync::Arc;
use std::time::Duration;

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
    #[cfg(unix)]
    let handle = PlatformHandle::from_raw_fd(libc::fileno(file));
    #[cfg(windows)]
    let handle =
        PlatformHandle::from_raw_handle(libc::get_osfhandle(libc::fileno(file)) as RawHandle);

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
pub extern "C" fn ddog_alloc_anon_shm_handle_named(
    size: usize,
    handle: &mut *mut ShmHandle,
    name: CharSlice,
) -> MaybeError {
    let name = name.to_utf8_lossy();
    *handle = Box::into_raw(Box::new(try_c!(ShmHandle::new_named(size, name.as_ref()))));

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
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_remote_config_reader_for_endpoint<'a>(
    language: &ffi::CharSlice<'a>,
    tracer_version: &ffi::CharSlice<'a>,
    endpoint: &Endpoint,
    service_name: ffi::CharSlice,
    env_name: ffi::CharSlice,
    app_version: ffi::CharSlice,
    tags: &ddcommon_ffi::Vec<Tag>,
    remote_config_products: *const RemoteConfigProduct,
    remote_config_products_count: usize,
    remote_config_capabilities: *const RemoteConfigCapabilities,
    remote_config_capabilities_count: usize,
) -> Box<RemoteConfigReader> {
    Box::new(RemoteConfigReader::new(
        &ConfigInvariants {
            language: language.to_utf8_lossy().into(),
            tracer_version: tracer_version.to_utf8_lossy().into(),
            endpoint: endpoint.clone(),
            products: slice::from_raw_parts(remote_config_products, remote_config_products_count)
                .to_vec(),
            capabilities: slice::from_raw_parts(
                remote_config_capabilities,
                remote_config_capabilities_count,
            )
            .to_vec(),
        },
        &Arc::new(Target {
            service: service_name.to_utf8_lossy().into(),
            env: env_name.to_utf8_lossy().into(),
            app_version: app_version.to_utf8_lossy().into(),
            tags: tags.as_slice().to_vec(),
        }),
    ))
}

/// # Safety
/// Argument should point to a valid C string.
#[no_mangle]
pub unsafe extern "C" fn ddog_remote_config_reader_for_path(
    path: *const c_char,
) -> Box<RemoteConfigReader> {
    Box::new(RemoteConfigReader::from_path(CStr::from_ptr(path)))
}

#[no_mangle]
extern "C" fn ddog_remote_config_path(
    id: *const ConfigInvariants,
    target: *const Arc<Target>,
) -> *mut c_char {
    let id = unsafe { &*id };
    let target = unsafe { &*target };
    path_for_remote_config(id, target).into_raw()
}
#[no_mangle]
unsafe extern "C" fn ddog_remote_config_path_free(path: *mut c_char) {
    drop(CString::from_raw(path));
}

#[no_mangle]
pub extern "C" fn ddog_remote_config_read<'a>(
    reader: &'a mut RemoteConfigReader,
    data: &mut ffi::CharSlice<'a>,
) -> bool {
    let (new, contents) = reader.read();
    // c_char may be u8 or i8 depending on target... convert it.
    let contents: &[c_char] = unsafe { std::mem::transmute::<&[u8], &[c_char]>(contents) };
    *data = contents.into();
    new
}

#[no_mangle]
pub extern "C" fn ddog_remote_config_reader_drop(_: Box<RemoteConfigReader>) {}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_drop(_: Box<SidecarTransport>) {}

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
pub extern "C" fn ddog_sidecar_flush_traces(transport: &mut Box<SidecarTransport>) -> MaybeError {
    try_c!(blocking::flush_traces(transport));

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
) -> Box<RuntimeMetadata> {
    let inner = RuntimeMetadata::new(
        language_name.to_utf8_lossy(),
        language_version.to_utf8_lossy(),
        tracer_version.to_utf8_lossy(),
    );

    Box::from(inner)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_runtimeMeta_drop(meta: Box<RuntimeMetadata>) {
    drop(meta)
}

/// Reports the runtime configuration to the telemetry.
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
        vec![SidecarAction::Telemetry(config_entry)],
    ));
    MaybeError::None
}

/// Reports a dependency to the telemetry.
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
        vec![SidecarAction::Telemetry(dependency)],
    ));

    MaybeError::None
}

/// Reports an integration to the telemetry.
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
        vec![SidecarAction::Telemetry(integration)],
    ));

    MaybeError::None
}

/// Registers a service and flushes any queued actions.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_flushServiceData(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    runtime_meta: &RuntimeMetadata,
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

/// Enqueues a list of actions to be performed.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_lifecycle_end(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
) -> MaybeError {
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![SidecarAction::Telemetry(TelemetryActions::Lifecycle(
            LifecycleAction::Stop
        ))],
    ));

    MaybeError::None
}

/// Flushes the telemetry data.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_flush(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
) -> MaybeError {
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![
            SidecarAction::Telemetry(TelemetryActions::Lifecycle(
                LifecycleAction::FlushMetricAggr
            )),
            SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::FlushData)),
        ],
    ));

    MaybeError::None
}

/// Returns whether the sidecar transport is closed or not.
#[no_mangle]
pub extern "C" fn ddog_sidecar_is_closed(transport: &mut Box<SidecarTransport>) -> bool {
    transport.is_closed()
}

/// Sets the configuration for a session.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_session_set_config(
    transport: &mut Box<SidecarTransport>,
    session_id: ffi::CharSlice,
    agent_endpoint: &Endpoint,
    dogstatsd_endpoint: &Endpoint,
    language: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
    flush_interval_milliseconds: u32,
    remote_config_poll_interval_millis: u32,
    telemetry_heartbeat_interval_millis: u32,
    force_flush_size: usize,
    force_drop_size: usize,
    log_level: ffi::CharSlice,
    log_path: ffi::CharSlice,
    #[allow(unused)] // On FFI layer we cannot conditionally compile, so we need the arg
    remote_config_notify_function: *mut c_void,
    remote_config_products: *const RemoteConfigProduct,
    remote_config_products_count: usize,
    remote_config_capabilities: *const RemoteConfigCapabilities,
    remote_config_capabilities_count: usize,
) -> MaybeError {
    #[cfg(unix)]
    let remote_config_notify_target = libc::getpid();
    #[cfg(windows)]
    let remote_config_notify_target = remote_config_notify_function;
    try_c!(blocking::set_session_config(
        transport,
        remote_config_notify_target,
        session_id.to_utf8_lossy().into(),
        &SessionConfig {
            endpoint: agent_endpoint.clone(),
            dogstatsd_endpoint: dogstatsd_endpoint.clone(),
            language: language.to_utf8_lossy().into(),
            tracer_version: tracer_version.to_utf8_lossy().into(),
            flush_interval: Duration::from_millis(flush_interval_milliseconds as u64),
            remote_config_poll_interval: Duration::from_millis(
                remote_config_poll_interval_millis as u64
            ),
            telemetry_heartbeat_interval: Duration::from_millis(
                telemetry_heartbeat_interval_millis as u64
            ),
            force_flush_size,
            force_drop_size,
            log_level: log_level.to_utf8_lossy().into(),
            log_file: if log_path.is_empty() {
                config::FromEnv::log_method()
            } else {
                LogMethod::File(String::from(log_path.to_utf8_lossy()).into())
            },
            remote_config_products: slice::from_raw_parts(
                remote_config_products,
                remote_config_products_count
            )
            .to_vec(),
            remote_config_capabilities: slice::from_raw_parts(
                remote_config_capabilities,
                remote_config_capabilities_count
            )
            .to_vec(),
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

impl<'a> TryInto<SerializedTracerHeaderTags> for &'a TracerHeaderTags<'a> {
    type Error = std::io::Error;

    fn try_into(self) -> Result<SerializedTracerHeaderTags, Self::Error> {
        let tags = datadog_trace_utils::trace_utils::TracerHeaderTags {
            lang: &self.lang.to_utf8_lossy(),
            lang_version: &self.lang_version.to_utf8_lossy(),
            lang_interpreter: &self.lang_interpreter.to_utf8_lossy(),
            lang_vendor: &self.lang_vendor.to_utf8_lossy(),
            tracer_version: &self.tracer_version.to_utf8_lossy(),
            container_id: &self.container_id.to_utf8_lossy(),
            client_computed_top_level: self.client_computed_top_level,
            client_computed_stats: self.client_computed_stats,
            ..Default::default()
        };

        tags.try_into().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Failed to convert TracerHeaderTags to SerializedTracerHeaderTags",
            )
        })
    }
}

/// Sends a trace to the sidecar via shared memory.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_send_trace_v04_shm(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    shm_handle: Box<ShmHandle>,
    len: usize,
    tracer_header_tags: &TracerHeaderTags,
) -> MaybeError {
    let tracer_header_tags = try_c!(tracer_header_tags.try_into());

    try_c!(blocking::send_trace_v04_shm(
        transport,
        instance_id,
        *shm_handle,
        len,
        tracer_header_tags,
    ));

    MaybeError::None
}

/// Sends a trace as bytes to the sidecar.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_send_trace_v04_bytes(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    data: ffi::CharSlice,
    tracer_header_tags: &TracerHeaderTags,
) -> MaybeError {
    let tracer_header_tags = try_c!(tracer_header_tags.try_into());

    try_c!(blocking::send_trace_v04_bytes(
        transport,
        instance_id,
        data.as_bytes().to_vec(),
        tracer_header_tags,
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
#[allow(improper_ctypes_definitions)] // DebuggerPayload is just a pointer, we hide its internals
pub unsafe extern "C" fn ddog_sidecar_send_debugger_data(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: QueueId,
    payloads: Vec<DebuggerPayload>,
) -> MaybeError {
    if payloads.is_empty() {
        return MaybeError::None;
    }

    try_c!(blocking::send_debugger_data_shm_vec(
        transport,
        instance_id,
        queue_id,
        payloads,
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
#[allow(improper_ctypes_definitions)] // DebuggerPayload is just a pointer, we hide its internals
pub unsafe extern "C" fn ddog_sidecar_send_debugger_datum(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: QueueId,
    payload: Box<DebuggerPayload>,
) -> MaybeError {
    ddog_sidecar_send_debugger_data(transport, instance_id, queue_id, vec![*payload])
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
#[allow(improper_ctypes_definitions)] // DebuggerPayload is just a pointer, we hide its internals
pub unsafe extern "C" fn ddog_sidecar_send_debugger_diagnostics(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: QueueId,
    diagnostics_payload: DebuggerPayload,
) -> MaybeError {
    try_c!(blocking::send_debugger_diagnostics(
        transport,
        instance_id,
        queue_id,
        diagnostics_payload,
    ));

    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_set_remote_config_data(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    service_name: ffi::CharSlice,
    env_name: ffi::CharSlice,
    app_version: ffi::CharSlice,
    global_tags: &ddcommon_ffi::Vec<Tag>,
) -> MaybeError {
    try_c!(blocking::set_remote_config_data(
        transport,
        instance_id,
        queue_id,
        service_name.to_utf8_lossy().into(),
        env_name.to_utf8_lossy().into(),
        app_version.to_utf8_lossy().into(),
        global_tags.to_vec(),
    ));

    MaybeError::None
}

/// Dumps the current state of the sidecar.
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

/// Retrieves the current statistics of the sidecar.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_stats(
    transport: &mut Box<SidecarTransport>,
) -> ffi::CharSlice {
    let str = match blocking::stats(transport) {
        Ok(stats) => stats,
        Err(e) => format!("{:?}", e),
    };
    let size = str.len();
    let malloced = libc::malloc(size) as *mut u8;
    let buf = slice::from_raw_parts_mut(malloced, size);
    buf.copy_from_slice(str.as_bytes());
    ffi::CharSlice::from_raw_parts(malloced as *mut c_char, size)
}

/// Send a DogStatsD "count" metric.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dogstatsd_count(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    metric: ffi::CharSlice,
    value: i64,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
) -> MaybeError {
    try_c!(blocking::send_dogstatsd_actions(
        transport,
        instance_id,
        vec![DogStatsDActionOwned::Count(
            metric.to_utf8_lossy().into_owned(),
            value,
            tags.map(|tags| tags.iter().cloned().collect())
                .unwrap_or_default()
        ),],
    ));

    MaybeError::None
}

/// Send a DogStatsD "distribution" metric.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dogstatsd_distribution(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    metric: ffi::CharSlice,
    value: f64,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
) -> MaybeError {
    try_c!(blocking::send_dogstatsd_actions(
        transport,
        instance_id,
        vec![DogStatsDActionOwned::Distribution(
            metric.to_utf8_lossy().into_owned(),
            value,
            tags.map(|tags| tags.iter().cloned().collect())
                .unwrap_or_default()
        ),],
    ));

    MaybeError::None
}

/// Send a DogStatsD "gauge" metric.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dogstatsd_gauge(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    metric: ffi::CharSlice,
    value: f64,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
) -> MaybeError {
    try_c!(blocking::send_dogstatsd_actions(
        transport,
        instance_id,
        vec![DogStatsDActionOwned::Gauge(
            metric.to_utf8_lossy().into_owned(),
            value,
            tags.map(|tags| tags.iter().cloned().collect())
                .unwrap_or_default()
        ),],
    ));

    MaybeError::None
}

/// Send a DogStatsD "histogram" metric.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dogstatsd_histogram(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    metric: ffi::CharSlice,
    value: f64,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
) -> MaybeError {
    try_c!(blocking::send_dogstatsd_actions(
        transport,
        instance_id,
        vec![DogStatsDActionOwned::Histogram(
            metric.to_utf8_lossy().into_owned(),
            value,
            tags.map(|tags| tags.iter().cloned().collect())
                .unwrap_or_default()
        ),],
    ));

    MaybeError::None
}

/// Send a DogStatsD "set" metric.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_dogstatsd_set(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    metric: ffi::CharSlice,
    value: i64,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
) -> MaybeError {
    try_c!(blocking::send_dogstatsd_actions(
        transport,
        instance_id,
        vec![DogStatsDActionOwned::Set(
            metric.to_utf8_lossy().into_owned(),
            value,
            tags.map(|tags| tags.iter().cloned().collect())
                .unwrap_or_default()
        ),],
    ));

    MaybeError::None
}

/// Sets x-datadog-test-session-token on all requests for the given session.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_set_test_session_token(
    transport: &mut Box<SidecarTransport>,
    session_id: ffi::CharSlice,
    token: ffi::CharSlice,
) -> MaybeError {
    try_c!(blocking::set_test_session_token(
        transport,
        session_id.to_utf8_lossy().into_owned(),
        token.to_utf8_lossy().into_owned(),
    ));

    MaybeError::None
}

/// This function creates a new transport using the provided callback function when the current
/// transport is closed.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `factory` - A C function that must return a pointer to "ddog_SidecarTransport"
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub extern "C" fn ddog_sidecar_reconnect(
    transport: &mut Box<SidecarTransport>,
    factory: unsafe extern "C" fn() -> Option<Box<SidecarTransport>>,
) {
    transport.reconnect(|| unsafe { factory() });
}

/// Return the path of the crashtracker unix domain socket.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_get_crashtracker_unix_socket_path() -> ffi::CharSlice<'static>
{
    let socket_path = crashtracker_unix_socket_path();
    let str = socket_path.to_str().unwrap_or_default();

    let size = str.len();
    let malloced = libc::malloc(size) as *mut u8;
    let buf = slice::from_raw_parts_mut(malloced, size);
    buf.copy_from_slice(str.as_bytes());
    ffi::CharSlice::from_raw_parts(malloced as *mut c_char, size)
}

/// Gets an agent info reader.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_agent_info_reader(endpoint: &Endpoint) -> Box<AgentInfoReader> {
    Box::new(AgentInfoReader::new(endpoint))
}

/// Gets the current agent info environment (or empty if not existing)
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_agent_info_env<'a>(
    reader: &'a mut AgentInfoReader,
    changed: &mut bool,
) -> ffi::CharSlice<'a> {
    let (has_changed, info) = reader.read();
    *changed = has_changed;
    let config = if let Some(info) = info {
        info.config.as_ref()
    } else {
        None
    };
    config
        .and_then(|c| c.default_env.as_ref())
        .map(|s| ffi::CharSlice::from(s.as_str()))
        .unwrap_or(ffi::CharSlice::empty())
}

/// Drops the agent info reader.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_drop_agent_info_reader(_: Box<AgentInfoReader>) {}
