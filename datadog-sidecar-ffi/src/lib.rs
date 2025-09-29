// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod span;

use crate::span::TracesBytes;
#[cfg(windows)]
use datadog_crashtracker_ffi::Metadata;
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
use datadog_sidecar::service::{get_telemetry_action_sender, InternalTelemetryActions};
use datadog_sidecar::shm_remote_config::{path_for_remote_config, RemoteConfigReader};
use datadog_trace_utils::msgpack_encoder;
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use ddcommon_ffi as ffi;
use ddcommon_ffi::{CharSlice, MaybeError};
use ddtelemetry::{
    data::{self, Dependency, Integration},
    worker::{LifecycleAction, LogIdentifier, TelemetryActions},
};
use ddtelemetry_ffi::try_c;
use dogstatsd_client::DogStatsDActionOwned;
use ffi::slice::AsBytes;
use libc::c_char;
use std::ffi::{c_void, CStr, CString};
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
#[cfg(unix)]
use std::os::unix::prelude::FromRawFd;
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::NonNull;
use std::slice;
use std::sync::Arc;
use std::time::Duration;
use serde_json::Value;


#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn ddog_setup_crashtracking(
    endpoint: Option<&Endpoint>,
    metadata: Metadata,
) -> bool {
    datadog_sidecar::ddog_setup_crashtracking(endpoint, metadata)
}

#[repr(C)]
pub struct NativeFile {
    pub handle: Box<PlatformHandle<File>>,
}

/// This creates Rust PlatformHandle<File> from supplied C std FILE object.
/// This method takes the ownership of the underlying file descriptor.
///
/// # Safety
/// Caller must ensure the file descriptor associated with FILE pointer is open, and valid
/// Caller must not close the FILE associated file descriptor after calling this function
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
    *data = CharSlice::from_bytes(contents);
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
    *data = CharSlice::from_bytes(contents);
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
    config_id: ffi::CharSlice,
) -> MaybeError {
    let config_id = if config_id.is_empty() {
        None
    } else {
        Some(config_id.to_utf8_lossy().into_owned())
    };
    let config_entry = TelemetryActions::AddConfig(data::Configuration {
        name: config_key.to_utf8_lossy().into_owned(),
        value: config_value.to_utf8_lossy().into_owned(),
        origin,
        config_id,
    });
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![SidecarAction::Telemetry(config_entry)],
    ));
    MaybeError::None
}

/// Reports an endpoint to the telemetry.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_telemetry_addEndpoint(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    r#type: CharSlice,
    method: ddtelemetry::data::Method,
    path: CharSlice,
    operation_name: CharSlice,
    resource_name: CharSlice,
    request_body_type:&mut ffi::Vec<CharSlice>,
    response_body_type:&mut ffi::Vec<CharSlice>,
    response_code:i32,
    authentication:&mut ffi::Vec<ddtelemetry::data::Authentication>,
    metadata: CharSlice
) -> MaybeError {

    let response_code_vec = vec![response_code];

    let metadata_json = serde_json::from_slice::<serde_json::Value>(&metadata.to_utf8_lossy().into_owned().as_bytes()).unwrap();
    let endpoint = TelemetryActions::AddEndpoint(ddtelemetry::data::Endpoint {
        r#type: Some(r#type.to_utf8_lossy().into_owned()),
        method: Some(method),
        path: Some(path.to_utf8_lossy().into_owned()),
        operation_name: operation_name.to_utf8_lossy().into_owned(),
        resource_name: resource_name.to_utf8_lossy().into_owned(),
        request_body_type: Some(request_body_type.to_vec().iter().map(|s| s.to_utf8_lossy().into_owned()).collect()),
        response_body_type: Some(response_body_type.to_vec().iter().map(|s| s.to_utf8_lossy().into_owned()).collect()),
        response_code: Some(response_code_vec),
        authentication: Some(authentication.to_vec()),
        metadata: Some(metadata_json),
    });

    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![SidecarAction::Telemetry(endpoint)],
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

    let dependency = TelemetryActions::AddDependency(Dependency {
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
        vec![
            SidecarAction::Telemetry(TelemetryActions::Lifecycle(LifecycleAction::Stop)),
            SidecarAction::ClearQueueId
        ],
    ));

    MaybeError::None
}

/// Enqueues a list of actions to be performed.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_application_remove(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
) -> MaybeError {
    try_c!(blocking::enqueue_actions(
        transport,
        instance_id,
        queue_id,
        vec![SidecarAction::ClearQueueId],
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
    language_version: ffi::CharSlice,
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
    remote_config_enabled: bool,
    is_fork: bool,
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
            language_version: language_version.to_utf8_lossy().into(),
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
            remote_config_products: ffi::Slice::from_raw_parts(
                remote_config_products,
                remote_config_products_count
            )
            .as_slice()
            .to_vec(),
            remote_config_capabilities: ffi::Slice::from_raw_parts(
                remote_config_capabilities,
                remote_config_capabilities_count
            )
            .as_slice()
            .to_vec(),
            remote_config_enabled,
        },
        is_fork
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

/// Enqueues a telemetry log action to be processed internally.
/// Non-blocking. Logs might be dropped if the internal queue is full.
///
/// # Safety
/// Pointers must be valid, strings must be null-terminated if not null.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_enqueue_telemetry_log(
    session_id_ffi: CharSlice,
    runtime_id_ffi: CharSlice,
    service_name_ffi: CharSlice,
    env_name_ffi: CharSlice,
    identifier_ffi: CharSlice,
    level: ddtelemetry::data::LogLevel,
    message_ffi: CharSlice,
    stack_trace_ffi: Option<NonNull<CharSlice>>,
    tags_ffi: Option<NonNull<CharSlice>>,
    is_sensitive: bool,
) -> MaybeError {
    try_c!(ddog_sidecar_enqueue_telemetry_log_impl(
        session_id_ffi,
        runtime_id_ffi,
        service_name_ffi,
        env_name_ffi,
        identifier_ffi,
        level,
        message_ffi,
        stack_trace_ffi,
        tags_ffi,
        is_sensitive,
    ));

    MaybeError::None
}

fn char_slice_to_string(slice: CharSlice) -> Result<String, String> {
    let cast_slice =
        unsafe { slice::from_raw_parts(slice.as_slice().as_ptr() as *const u8, slice.len()) };
    let slice = std::str::from_utf8(cast_slice)
        .map_err(|e| format!("Failed to convert CharSlice to String: {e}"))?;
    Ok(slice.to_string())
}

#[allow(clippy::too_many_arguments)]
fn ddog_sidecar_enqueue_telemetry_log_impl(
    session_id_ffi: CharSlice,
    runtime_id_ffi: CharSlice,
    service_name_ffi: CharSlice,
    env_name_ffi: CharSlice,
    identifier_ffi: CharSlice,
    level: ddtelemetry::data::LogLevel,
    message_ffi: CharSlice,
    stack_trace_ffi: Option<NonNull<CharSlice>>,
    tags_ffi: Option<NonNull<CharSlice>>,
    is_sensitive: bool,
) -> Result<(), String> {
    if session_id_ffi.is_empty()
        || runtime_id_ffi.is_empty()
        || service_name_ffi.is_empty()
        || env_name_ffi.is_empty()
        || identifier_ffi.is_empty()
        || message_ffi.is_empty()
    {
        return Err("Null or empty required arguments".into());
    }

    let sender = match get_telemetry_action_sender() {
        Ok(s) => s,
        Err(e) => {
            return Err(format!("Failed to get telemetry action sender: {e}"));
        }
    };

    let instance_id = InstanceId::new(
        char_slice_to_string(session_id_ffi)?,
        char_slice_to_string(runtime_id_ffi)?,
    );
    let service_name: String = char_slice_to_string(service_name_ffi)?;
    let env_name: String = char_slice_to_string(env_name_ffi)?;
    let identifier: String = char_slice_to_string(identifier_ffi)?;
    let message: String = char_slice_to_string(message_ffi)?;

    let stack_trace = stack_trace_ffi
        .map(|s| char_slice_to_string(*unsafe { s.as_ref() }))
        .transpose()?;
    let tags: Option<String> = tags_ffi
        .map(|s| char_slice_to_string(*unsafe { s.as_ref() }))
        .transpose()?;

    let mut hasher = DefaultHasher::new();
    identifier.hash(&mut hasher);
    let log_id = LogIdentifier {
        identifier: hasher.finish(),
    };

    let log_data = ddtelemetry::data::Log {
        message,
        level,
        stack_trace,
        count: 1,
        tags: tags.unwrap_or("".into()),
        is_sensitive,
        is_crash: false,
    };
    let log_action = TelemetryActions::AddLog((log_id, log_data));

    let msg = InternalTelemetryActions {
        instance_id,
        service_name,
        env_name,
        actions: vec![log_action],
    };

    match sender.try_send(msg) {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("Failed to send telemetry action: {err}")),
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
pub unsafe extern "C" fn ddog_sidecar_set_universal_service_tags(
    transport: &mut Box<SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    service_name: ffi::CharSlice,
    env_name: ffi::CharSlice,
    app_version: ffi::CharSlice,
    global_tags: &ddcommon_ffi::Vec<Tag>,
) -> MaybeError {
    try_c!(blocking::set_universal_service_tags(
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
) -> ffi::CharSlice<'_> {
    let str = match blocking::dump(transport) {
        Ok(dump) => dump,
        Err(e) => format!("{e:?}"),
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
) -> ffi::CharSlice<'_> {
    let str = match blocking::stats(transport) {
        Ok(stats) => stats,
        Err(e) => format!("{e:?}"),
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

#[macro_export]
macro_rules! check {
    ($failable:expr, $msg:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("{}: {}", $msg, e);
                return;
            }
        }
    };
}

#[repr(C)]
#[derive()]
pub struct SenderParameters {
    pub tracer_headers_tags: TracerHeaderTags<'static>,
    pub transport: Box<SidecarTransport>,
    pub instance_id: Box<InstanceId>,
    pub limit: usize,
    pub n_requests: i64,
    pub buffer_size: i64,
    pub url: CharSlice<'static>,
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_send_traces_to_sidecar(
    traces: &mut TracesBytes,
    parameters: &mut SenderParameters,
) {
    let size: usize = traces.iter().map(|trace| trace.len()).sum();

    // Check connection to the sidecar
    if parameters.transport.is_closed() {
        tracing::info!(
            "Skipping flushing traces of size {} as connection to sidecar failed",
            size
        );
        return;
    }

    // Create and map shared memory
    let shm = check!(
        ShmHandle::new(parameters.limit),
        "Failed to create shared memory"
    );

    let mut mapped_shm = check!(shm.clone().map(), "Failed to map shared memory");

    // Write traces to the shared memory
    let mut shm_slice = mapped_shm.as_slice_mut();
    let shm_slice_len = shm_slice.len();
    let written = match msgpack_encoder::v04::write_to_slice(&mut shm_slice, traces) {
        Ok(()) => shm_slice_len - shm_slice.len(),
        Err(_) => {
            tracing::error!("Failed serializing the traces");
            return;
        }
    };

    // Send traces to the sidecar via the shared memory handler
    let mut size_hint = written;
    if parameters.n_requests > 0 {
        size_hint = size_hint.max((parameters.buffer_size / parameters.n_requests + 1) as usize);
    }

    let send_error = blocking::send_trace_v04_shm(
        &mut parameters.transport,
        &parameters.instance_id,
        shm,
        size_hint,
        check!(
            (&parameters.tracer_headers_tags).try_into(),
            "Failed to convert tracer headers tags"
        ),
    );

    // Retry sending traces via bytes if there was an error
    if send_error.is_err() {
        match blocking::send_trace_v04_bytes(
            &mut parameters.transport,
            &parameters.instance_id,
            msgpack_encoder::v04::to_vec_with_capacity(traces, written as u32),
            check!(
                (&parameters.tracer_headers_tags).try_into(),
                "Failed to convert tracer headers tags"
            ),
        ) {
            Ok(_) => {}
            Err(_) => tracing::debug!(
                "Failed sending traces via shm to sidecar: {}",
                send_error.err().unwrap_unchecked().to_string()
            ),
        };
    }

    tracing::event!(target: "info", tracing::Level::INFO, "Flushing trace of size {} to send-queue for {}", size, parameters.url);
    // tracing::info!(
    //     "Flushing traces of size {} to send-queue for {}",
    //     size,
    //     parameters.url
    // );
}

/// Drops the agent info reader.
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_drop_agent_info_reader(_: Box<AgentInfoReader>) {}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_sidecar_send_garbage(transport: &mut Box<SidecarTransport>) {
    // This shall fail.
    let _ = transport.send_garbage();
}
