// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ffi::slice::AsBytes;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common_ffi as ffi;
use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime, SharedRuntime, WorkerHandle};
use libdd_telemetry::{
    config, data,
    worker::{TelemetryWorkerBuilder, TelemetryWorkerFlavor},
};
use std::ptr::NonNull;
use std::sync::Arc;

/// FFI-facing alias: the C ABI surface is native-only, so the worker handle is
/// always pinned to [`NativeCapabilities`].
type TelemetryWorkerHandle = libdd_telemetry::worker::TelemetryWorkerHandle<NativeCapabilities>;

use ffi::MaybeError;

#[cfg(not(feature = "expanded_builder_macros"))]
mod macros;
#[cfg(not(feature = "expanded_builder_macros"))]
pub use macros::*;

#[cfg(feature = "expanded_builder_macros")]
mod expanded;
#[cfg(feature = "expanded_builder_macros")]
pub use expanded::*;

use crate::try_c;

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_builder_instantiate(
    out_builder: NonNull<Box<TelemetryWorkerBuilder>>,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    unsafe {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            service_name.to_utf8_lossy().into_owned(),
            language_name.to_utf8_lossy().into_owned(),
            language_version.to_utf8_lossy().into_owned(),
            tracer_version.to_utf8_lossy().into_owned(),
        );
        // This is not great but maintains compatibility code remove in Builder::run
        builder.config = libdd_telemetry::config::Config::from_env();

        let new = Box::new(builder);
        out_builder.as_ptr().write(new);
        MaybeError::None
    }
}

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_builder_instantiate_with_hostname(
    out_builder: NonNull<Box<TelemetryWorkerBuilder>>,
    hostname: ffi::CharSlice,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    unsafe {
        let mut builder = TelemetryWorkerBuilder::new(
            hostname.to_utf8_lossy().into_owned(),
            service_name.to_utf8_lossy().into_owned(),
            language_name.to_utf8_lossy().into_owned(),
            language_version.to_utf8_lossy().into_owned(),
            tracer_version.to_utf8_lossy().into_owned(),
        );
        // This is not great but maintains compatibility code remove in Builder::run
        builder.config = libdd_telemetry::config::Config::from_env();

        let new = Box::new(builder);
        out_builder.as_ptr().write(new);
        MaybeError::None
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_builder_with_native_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_native_deps: bool,
) -> MaybeError {
    builder.native_deps = include_native_deps;
    MaybeError::None
}

#[unsafe(no_mangle)]
/// Bounds how long `ddog_telemetry_worker_stop` waits for the final flush: once `timeout_ms`
/// elapses, in-flight requests are cancelled. `0` waits indefinitely. Defaults to 1000 ms.
// allow(ffi-panic-boundary): plain field assignment, cannot panic.
pub extern "C" fn ddog_telemetry_builder_with_shutdown_timeout_ms(
    builder: &mut TelemetryWorkerBuilder,
    timeout_ms: u64,
) -> MaybeError {
    builder.config.shutdown_timeout =
        (timeout_ms != 0).then(|| std::time::Duration::from_millis(timeout_ms));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_builder_with_rust_shared_lib_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_rust_shared_lib_deps: bool,
) -> MaybeError {
    builder.rust_shared_lib_deps = include_rust_shared_lib_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_builder_with_config(
    builder: &mut TelemetryWorkerBuilder,
    name: ffi::CharSlice,
    value: ffi::CharSlice,
    origin: data::ConfigurationOrigin,
    config_id: ffi::CharSlice,
    seq_id: ffi::Option<u64>,
) -> MaybeError {
    let name = name.to_utf8_lossy().into_owned();
    let value = value.to_utf8_lossy().into_owned();
    let config_id = if config_id.is_empty() {
        None
    } else {
        Some(config_id.to_utf8_lossy().into_owned())
    };
    let seq_id = seq_id.to_std();
    builder.configurations.insert(data::Configuration {
        name,
        value: Some(value),
        origin,
        config_id,
        seq_id,
    });
    MaybeError::None
}

/// A telemetry worker running on a shared runtime. Created by `ddog_telemetry_builder_spawn`,
/// released by `ddog_telemetry_worker_stop` or `ddog_telemetry_worker_drop`.
pub struct TelemetryWorkerRuntimeHandle {
    worker: WorkerHandle,
    telemetry: TelemetryWorkerHandle,
    runtime: Arc<ForkSafeRuntime>,
}

fn panic_error(context: &str) -> MaybeError {
    MaybeError::Some(ffi::Error::from(format!("{context} panicked")))
}

unsafe fn spawn_on_runtime(
    builder: Box<TelemetryWorkerBuilder>,
    runtime: *const ForkSafeRuntime,
    restart_on_fork: bool,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
    out_worker: NonNull<Box<TelemetryWorkerRuntimeHandle>>,
) -> MaybeError {
    let Some(runtime) = NonNull::new(runtime.cast_mut()) else {
        return MaybeError::Some(ffi::Error::from("runtime must not be null".to_string()));
    };
    // SAFETY: runtime was produced by Arc::into_raw (ddog_shared_runtime_new) and is alive.
    // Take our own strong reference so the caller still owns and frees its handle.
    let runtime = unsafe {
        Arc::increment_strong_count(runtime.as_ptr());
        Arc::from_raw(runtime.as_ptr())
    };
    let (telemetry, worker) = builder.build_worker::<NativeCapabilities>();
    let worker = crate::try_c!(runtime.spawn_worker(worker, restart_on_fork));
    unsafe {
        out_handle.as_ptr().write(Box::new(telemetry.clone()));
        out_worker
            .as_ptr()
            .write(Box::new(TelemetryWorkerRuntimeHandle {
                worker,
                telemetry,
                runtime,
            }));
    }
    MaybeError::None
}

#[unsafe(no_mangle)]
/// Builds the telemetry worker, spawns it on `runtime` and returns a handle to send telemetry
/// (`out_handle`) and a handle to stop the worker (`out_worker`).
///
/// The builder is consumed. The caller keeps ownership of `runtime` and must still free it.
///
/// # Safety
/// * `runtime` must be a valid handle from `ddog_shared_runtime_new`.
/// * `out_handle` and `out_worker` should be non null pointers to null pointers.
pub unsafe extern "C" fn ddog_telemetry_builder_spawn(
    builder: Box<TelemetryWorkerBuilder>,
    runtime: *const ForkSafeRuntime,
    restart_on_fork: bool,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
    out_worker: NonNull<Box<TelemetryWorkerRuntimeHandle>>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        spawn_on_runtime(builder, runtime, restart_on_fork, out_handle, out_worker)
    }))
    .unwrap_or_else(|_| panic_error("ddog_telemetry_builder_spawn"))
}

#[unsafe(no_mangle)]
/// Same as `ddog_telemetry_builder_spawn`, but the worker only processes and sends telemetry
/// metrics and logs. Any lifecycle/dependency/configuration event is ignored.
///
/// # Safety
/// Same as `ddog_telemetry_builder_spawn`.
pub unsafe extern "C" fn ddog_telemetry_builder_spawn_metric_logs(
    mut builder: Box<TelemetryWorkerBuilder>,
    runtime: *const ForkSafeRuntime,
    restart_on_fork: bool,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
    out_worker: NonNull<Box<TelemetryWorkerRuntimeHandle>>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        builder.flavor = TelemetryWorkerFlavor::MetricsLogs;
        spawn_on_runtime(builder, runtime, restart_on_fork, out_handle, out_worker)
    }))
    .unwrap_or_else(|_| panic_error("ddog_telemetry_builder_spawn_metric_logs"))
}

#[unsafe(no_mangle)]
/// Stops the worker, blocking until queued actions are processed and the final flush is done.
/// The wait is bounded by the builder's shutdown timeout (see
/// `ddog_telemetry_builder_with_shutdown_timeout_ms`). Must not be called from a runtime thread.
///
/// Takes ownership of `worker`: the pointer is invalid after every call, including when an
/// error is returned. Callers that already shut the runtime down must call
/// `ddog_telemetry_worker_drop` instead of this function.
pub extern "C" fn ddog_telemetry_worker_stop(
    worker: Box<TelemetryWorkerRuntimeHandle>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let TelemetryWorkerRuntimeHandle {
            worker,
            telemetry,
            runtime,
        } = *worker;
        let res = crate::try_c!(runtime.block_on(telemetry.stop_worker(worker)));
        crate::try_c!(res);
        MaybeError::None
    }))
    .unwrap_or_else(|_| panic_error("ddog_telemetry_worker_stop"))
}

#[unsafe(no_mangle)]
/// Frees the worker handle without stopping the worker. The worker keeps running until the
/// shared runtime is shut down (`ddog_shared_runtime_shutdown` also flushes it).
///
/// Takes ownership of `worker`; the pointer is invalid after this call. Returns an error if
/// releasing the last runtime reference panicked (e.g. when called from a runtime thread).
pub extern "C" fn ddog_telemetry_worker_drop(
    worker: Box<TelemetryWorkerRuntimeHandle>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        drop(worker);
        MaybeError::None
    }))
    .unwrap_or_else(|_| panic_error("ddog_telemetry_worker_drop"))
}

/// C-facing companion to [`libdd_telemetry::config::TelemetryEndpoint`]: the same
/// shape, but with caller-owned [`ffi::CharSlice`] strings instead of `String`s,
/// so `libdd_common::Endpoint` stays out of this crate's public API.
///
/// Empty `url`/`api_key`/`test_token` slices are treated as unset (leave the
/// existing value unchanged); a `timeout_ms` of 0 keeps the existing/default
/// timeout. `use_system_resolver` is always applied.
#[repr(C)]
pub struct TelemetryEndpoint<'a> {
    pub url: ffi::CharSlice<'a>,
    pub api_key: ffi::CharSlice<'a>,
    pub timeout_ms: u64,
    pub test_token: ffi::CharSlice<'a>,
    pub use_system_resolver: bool,
}

impl TelemetryEndpoint<'_> {
    /// Copies the caller-owned slices into an owned, `'static`
    /// [`config::TelemetryEndpoint`]. The copy is mandatory: the slices point at
    /// memory the C caller may free, so it cannot be borrowed into the config.
    ///
    /// An inherent method rather than a `From` impl because the orphan rule
    /// forbids implementing `From<Self> for` the foreign `config::TelemetryEndpoint`.
    fn into_config(self) -> config::TelemetryEndpoint {
        fn owned(slice: ffi::CharSlice) -> Option<String> {
            (!slice.is_empty()).then(|| slice.to_utf8_lossy().into_owned())
        }
        config::TelemetryEndpoint {
            url: owned(self.url),
            api_key: owned(self.api_key),
            test_token: owned(self.test_token),
            timeout_ms: self.timeout_ms,
            use_system_resolver: self.use_system_resolver,
        }
    }
}

/// Applies endpoint settings to the builder's telemetry config.
fn set_builder_endpoint(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    endpoint: TelemetryEndpoint,
) -> ffi::MaybeError {
    try_c!(
        telemetry_builder
            .config
            .set_endpoint(endpoint.into_config())
    );
    ffi::MaybeError::None
}

#[unsafe(no_mangle)]
#[allow(clippy::missing_safety_doc)]
/// Sets the telemetry endpoint from its component parts.
///
/// * `api_key` / `test_token`: ignored when empty.
/// * `timeout_ms`: pass 0 to keep the existing/default timeout.
pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_config_endpoint(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    url: ffi::CharSlice,
    api_key: ffi::CharSlice,
    timeout_ms: u64,
    test_token: ffi::CharSlice,
    use_system_resolver: bool,
) -> ffi::MaybeError {
    set_builder_endpoint(
        telemetry_builder,
        TelemetryEndpoint {
            url,
            api_key,
            timeout_ms,
            test_token,
            use_system_resolver,
        },
    )
}
#[repr(C)]
#[allow(dead_code)]
pub enum TelemetryWorkerBuilderEndpointProperty {
    ConfigEndpoint,
}

#[unsafe(no_mangle)]
#[allow(clippy::missing_safety_doc)]
/// Sets the endpoint property from its component parts.
///
/// Available properties:
///
/// * config.endpoint
pub unsafe extern "C" fn ddog_telemetry_builder_with_property_endpoint(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    _property: TelemetryWorkerBuilderEndpointProperty,
    url: ffi::CharSlice,
    api_key: ffi::CharSlice,
    timeout_ms: u64,
    test_token: ffi::CharSlice,
    use_system_resolver: bool,
) -> ffi::MaybeError {
    set_builder_endpoint(
        telemetry_builder,
        TelemetryEndpoint {
            url,
            api_key,
            timeout_ms,
            test_token,
            use_system_resolver,
        },
    )
}
#[unsafe(no_mangle)]
#[allow(clippy::missing_safety_doc)]
/// Sets a named endpoint property from its component parts.
///
/// Available properties:
///
/// * config.endpoint
pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_named_property(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    property: ffi::CharSlice,
    url: ffi::CharSlice,
    api_key: ffi::CharSlice,
    timeout_ms: u64,
    test_token: ffi::CharSlice,
    use_system_resolver: bool,
) -> ffi::MaybeError {
    let property = try_c!(property.try_to_utf8());

    match property {
        "config . endpoint" => set_builder_endpoint(
            telemetry_builder,
            TelemetryEndpoint {
                url,
                api_key,
                timeout_ms,
                test_token,
                use_system_resolver,
            },
        ),
        _ => ffi::MaybeError::None,
    }
}
