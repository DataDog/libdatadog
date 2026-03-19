// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI symbols that the AppSec helper resolves via `dlsym(RTLD_DEFAULT, …)`
//! from within the sidecar process.
//!
//! This crate is linked *only* into `datadog-ipc-helper`.  It must never be
//! linked into `ddtrace.so` (use `datadog-sidecar-ffi` for that side).
//!
//! Three additional symbols are satisfied by other already-linked crates:
//! * `ddog_set_rc_notify_fn`  – defined in `datadog-sidecar/src/shm_remote_config.rs`
//! * `ddog_Error_drop`        – defined in `libdd-common-ffi/src/error.rs`
//! * `ddog_Error_message`     – defined in `libdd-common-ffi/src/error.rs`

use datadog_remote_config::fetch::ConfigInvariants;
use datadog_remote_config::Target;
use datadog_sidecar::config;
use datadog_sidecar::service::blocking::{self, SidecarTransport};
use datadog_sidecar::service::telemetry::InternalTelemetryAction;
use datadog_sidecar::service::{get_telemetry_action_sender, InstanceId, InternalTelemetryActions};
use datadog_sidecar::setup::{DefaultLiason, Liaison};
use datadog_sidecar::shm_remote_config::path_for_remote_config;
use libc::c_char;
use libdd_common_ffi::slice::{AsBytes, CharSlice};
// `try_c!` from libdd-telemetry-ffi references `ffi::MaybeError`; provide the alias it expects.
use libdd_common_ffi::{self as ffi, MaybeError};
use libdd_telemetry::data::metrics::{MetricNamespace, MetricType};
use libdd_telemetry::metrics::MetricContext;
use libdd_telemetry::worker::{LogIdentifier, TelemetryActions};
use libdd_telemetry_ffi::try_c;
use std::ffi::CString;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ptr::NonNull;
use std::sync::Arc;

/// Connect to an already-running sidecar.  **Never** tries to spawn one.
/// Used by the AppSec helper's `SidecarReadyFuture` from within the sidecar
/// process to verify the sidecar is accepting connections.
#[no_mangle]
pub extern "C" fn ddog_sidecar_connect(connection: &mut *mut SidecarTransport) -> MaybeError {
    let cfg = config::FromEnv::config();
    let liaison = match cfg.ipc_mode {
        config::IpcMode::Shared => DefaultLiason::ipc_shared(),
        config::IpcMode::InstancePerProcess => DefaultLiason::ipc_per_process(),
    };
    let stream = Box::new(try_c!(liaison
        .connect_to_server()
        .map(SidecarTransport::from)
        .map_err(|e| e.to_string())));
    *connection = Box::into_raw(stream);
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_transport_drop(_: Box<SidecarTransport>) {}

#[no_mangle]
pub extern "C" fn ddog_sidecar_ping(transport: &mut Box<SidecarTransport>) -> MaybeError {
    try_c!(blocking::ping(transport));
    MaybeError::None
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

fn char_slice_to_string(slice: CharSlice) -> Result<String, String> {
    slice
        .try_to_string()
        .map_err(|e| format!("Failed to convert CharSlice to String: {e}"))
}

struct TelemetryContext {
    instance_id: InstanceId,
    service_name: String,
    env_name: String,
}

impl TelemetryContext {
    fn from_ffi(
        session_id: CharSlice,
        runtime_id: CharSlice,
        service_name: CharSlice,
        env_name: CharSlice,
    ) -> Result<Self, String> {
        if session_id.is_empty() {
            return Err("empty session_id".into());
        }
        if runtime_id.is_empty() {
            return Err("empty runtime_id".into());
        }
        if service_name.is_empty() {
            return Err("empty service_name".into());
        }
        if env_name.is_empty() {
            return Err("empty env_name".into());
        }
        Ok(Self {
            instance_id: InstanceId::new(
                char_slice_to_string(session_id)?,
                char_slice_to_string(runtime_id)?,
            ),
            service_name: char_slice_to_string(service_name)?,
            env_name: char_slice_to_string(env_name)?,
        })
    }

    fn send_action(self, action: InternalTelemetryAction) -> Result<(), String> {
        let sender = get_telemetry_action_sender()
            .map_err(|e| format!("Failed to get telemetry action sender: {e}"))?;
        sender
            .try_send(InternalTelemetryActions {
                instance_id: self.instance_id,
                service_name: self.service_name,
                env_name: self.env_name,
                actions: vec![action],
            })
            .map_err(|e| format!("Failed to send telemetry action: {e}"))
    }
}

/// # Safety
/// Pointers must be valid; strings must be non-null.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ddog_sidecar_enqueue_telemetry_log(
    session_id: CharSlice,
    runtime_id: CharSlice,
    service_name: CharSlice,
    env_name: CharSlice,
    identifier: CharSlice,
    level: libdd_telemetry::data::LogLevel,
    message: CharSlice,
    stack_trace: Option<NonNull<CharSlice>>,
    tags: Option<NonNull<CharSlice>>,
    is_sensitive: bool,
) -> MaybeError {
    if identifier.is_empty() || message.is_empty() {
        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
            "empty identifier or message".to_owned(),
        ));
    }
    let ctx = try_c!(TelemetryContext::from_ffi(
        session_id,
        runtime_id,
        service_name,
        env_name
    ));
    let id_str = try_c!(char_slice_to_string(identifier));
    let msg_str = try_c!(char_slice_to_string(message));
    let stack = match stack_trace {
        Some(p) => Some(try_c!(char_slice_to_string(*p.as_ref()))),
        None => None,
    };
    let tags_str = match tags {
        Some(p) => Some(try_c!(char_slice_to_string(*p.as_ref()))),
        None => None,
    };
    let mut hasher = DefaultHasher::new();
    id_str.hash(&mut hasher);
    let log_id = LogIdentifier {
        identifier: hasher.finish(),
    };
    let log_data = libdd_telemetry::data::Log {
        message: msg_str,
        level,
        stack_trace: stack,
        count: 1,
        tags: tags_str.unwrap_or_default(),
        is_sensitive,
        is_crash: false,
    };
    try_c!(ctx.send_action(InternalTelemetryAction::TelemetryAction(
        TelemetryActions::AddLog((log_id, log_data))
    )));
    MaybeError::None
}

/// # Safety
/// Pointers must be valid; strings must be non-null.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_enqueue_telemetry_point(
    session_id: CharSlice,
    runtime_id: CharSlice,
    service_name: CharSlice,
    env_name: CharSlice,
    metric_name: CharSlice,
    value: f64,
    tags: Option<NonNull<CharSlice>>,
) -> MaybeError {
    if metric_name.is_empty() {
        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
            "empty metric_name".to_owned(),
        ));
    }
    let ctx = try_c!(TelemetryContext::from_ffi(
        session_id,
        runtime_id,
        service_name,
        env_name
    ));
    let name = try_c!(char_slice_to_string(metric_name));

    let tag_vec = match tags {
        Some(p) => {
            let s = try_c!(char_slice_to_string(*p.as_ref()));
            let (parsed, err) = libdd_common::tag::parse_tags(s.as_str());
            if let Some(e) = err {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(e.to_string()));
            }
            parsed
        }
        None => Vec::new(),
    };
    try_c!(ctx.send_action(InternalTelemetryAction::AddMetricPoint((
        value, name, tag_vec
    ))));
    MaybeError::None
}

/// # Safety
/// Pointers must be valid; strings must be non-null.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_enqueue_telemetry_metric(
    session_id: CharSlice,
    runtime_id: CharSlice,
    service_name: CharSlice,
    env_name: CharSlice,
    metric_name: CharSlice,
    metric_type: MetricType,
    metric_namespace: MetricNamespace,
) -> MaybeError {
    if metric_name.is_empty() {
        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
            "empty metric_name".to_owned(),
        ));
    }
    let ctx = try_c!(TelemetryContext::from_ffi(
        session_id,
        runtime_id,
        service_name,
        env_name
    ));
    let name = try_c!(char_slice_to_string(metric_name));
    try_c!(
        ctx.send_action(InternalTelemetryAction::RegisterTelemetryMetric(
            MetricContext {
                name,
                tags: Vec::new(),
                metric_type,
                common: true,
                namespace: metric_namespace,
            },
        ))
    );
    MaybeError::None
}
