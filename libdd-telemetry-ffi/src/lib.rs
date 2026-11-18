// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod builder;
pub mod worker_handle;

#[allow(unused_macros)]
macro_rules! c_setters {
    (
        object_name => $object_name:ident,
        object_type => $object_ty:ty,
        property_type => $property_type:ty,
        property_type_name_snakecase => $property_type_name_snakecase:ident,
        property_type_name_camel_case => $property_type_name_camel_case:ident,
        convert_fn => $convert_fn:expr,
        SETTERS { $($path:ident $(. $path_rest:ident)*),+ $(,)? }
    ) => {
        paste::paste! {
            $(
                #[unsafe(no_mangle)]
                #[allow(clippy::missing_safety_doc)]
                pub unsafe extern "C" fn [<ddog_ $object_name _with_ $property_type_name_snakecase _ $path $(_ $path_rest)* >](
                    $object_name: &mut $object_ty,
                    param: $property_type,
                ) -> ffi::MaybeError {
                    $object_name . $path $(.  $path_rest)* = crate::try_c!($convert_fn (param));
                    ffi::MaybeError::None
                }
            )+

            #[repr(C)]
            #[allow(dead_code)]
            pub enum [<$object_ty $property_type_name_camel_case Property >] {
                $([< $path:camel $($path_rest:camel)* >],)+
            }

            #[unsafe(no_mangle)]
            #[allow(clippy::missing_safety_doc)]
            #[doc=concat!(
                "\n Sets a property from it's string value.\n\n",
                " Available properties:\n\n",
                $(" * ", stringify!($path $(. $path_rest)*) , "\n\n",)+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_property_ $property_type_name_snakecase>](
                $object_name: &mut $object_ty,
                property: [<$object_ty $property_type_name_camel_case Property >],
                param: $property_type,
            ) -> ffi::MaybeError {
                use [<$object_ty $property_type_name_camel_case Property >] ::*;
                match property {
                    $(
                        [< $path:camel $($path_rest:camel)* >] => {
                            $object_name . $path $(.  $path_rest)* = crate::try_c!($convert_fn (param));
                        }
                    )+
                }
                ffi::MaybeError::None
            }

            #[unsafe(no_mangle)]
            #[allow(clippy::missing_safety_doc)]
            #[doc=concat!(
                "\n Sets a property from it's string value.\n\n",
                " Available properties:\n\n",
                $(
                    " * ", stringify!($path $(. $path_rest)*) , "\n\n",
                )+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_ $property_type_name_snakecase _named_property>](
                $object_name: &mut $object_ty,
                property: ffi::CharSlice,
                param: $property_type,
            ) -> ffi::MaybeError {
                let property = crate::try_c!(property.try_to_utf8());
                match property {
                    $(
                        stringify!($path $(. $path_rest)*) => {
                            $object_name . $path $(.  $path_rest)* = crate::try_c!($convert_fn (param));
                        }
                    )+
                    // TODO this is an error
                    _ => return ffi::MaybeError::None,
                }
                ffi::MaybeError::None
            }
        }

    };
}

#[macro_export]
macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return ffi::MaybeError::Some(libdd_common_ffi::Error::from(format!("{e:?}"))),
        }
    };
}

/// Runs `$body` (which evaluates to a `MaybeError`) and turns a panic into an error return,
/// so it never unwinds across the FFI boundary.
///
/// Differs from the `catch_panic!` in `libdd-data-pipeline-ffi`, which takes an explicit error
/// value, only logs the panic message through `tracing`, and is a no-op without its
/// `catch_panic` feature. This one always catches, and returns the panic message to the caller in
/// the `MaybeError`. `wrap_with_void_ffi_result!` from `libdd-common-ffi` isn't used because it
/// returns a `VoidResult`, while the telemetry handle API returns `MaybeError`.
macro_rules! catch_panic {
    ($body:block) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> ffi::MaybeError {
            $body
        })) {
            Ok(ret) => ret,
            Err(info) => {
                let msg = if let Some(s) = info.downcast_ref::<String>() {
                    s.as_str()
                } else if let Some(s) = info.downcast_ref::<&str>() {
                    s
                } else {
                    "unable to retrieve panic context"
                };
                ffi::MaybeError::Some(libdd_common_ffi::Error::from(format!(
                    "{} panicked: {msg}",
                    function_name!()
                )))
            }
        }
    };
}

#[allow(unused_imports)]
pub(crate) use c_setters;
pub(crate) use catch_panic;

#[cfg(test)]
mod tests {
    use crate::{builder::*, worker_handle::*};
    use ffi::MaybeError;
    use ffi::tags::{PushTagResult, ddog_Vec_Tag_new, ddog_Vec_Tag_push};
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_common_ffi as ffi;
    use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
    use libdd_telemetry::{
        data::metrics::{MetricNamespace, MetricType},
        worker::TelemetryWorkerBuilder,
    };
    use std::{mem::MaybeUninit, ptr::NonNull, sync::Arc};

    /// Spawns `builder` on a fresh ForkSafeRuntime through the FFI. The returned worker handle
    /// keeps the runtime alive.
    pub(crate) unsafe fn spawn_worker(
        builder: Box<TelemetryWorkerBuilder>,
        metric_logs: bool,
    ) -> (
        Box<TelemetryWorkerHandle>,
        Box<TelemetryWorkerRuntimeHandle>,
    ) {
        unsafe {
            let runtime = Arc::into_raw(Arc::new(ForkSafeRuntime::new().unwrap()));
            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            let mut worker: MaybeUninit<Box<TelemetryWorkerRuntimeHandle>> = MaybeUninit::uninit();
            let spawn = if metric_logs {
                ddog_telemetry_builder_spawn_metric_logs
            } else {
                ddog_telemetry_builder_spawn
            };
            spawn(
                builder,
                runtime,
                false,
                NonNull::new(&mut handle).unwrap().cast(),
                NonNull::new(&mut worker).unwrap().cast(),
            )
            .unwrap_none();
            // The caller's reference is released; the worker holds its own.
            drop(Arc::from_raw(runtime));
            (handle.assume_init(), worker.assume_init())
        }
    }

    /// Stops the worker and releases both handles.
    pub(crate) fn stop_worker(
        handle: Box<TelemetryWorkerHandle>,
        worker: Box<TelemetryWorkerRuntimeHandle>,
    ) {
        ddog_telemetry_worker_stop(worker).unwrap_none();
        ddog_telemetry_handle_drop(handle);
    }

    /// Dropping the last runtime reference from an async context makes tokio panic; the FFI
    /// must report it as an error instead of unwinding across the boundary.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_drop_in_async_context_is_contained() {
        unsafe {
            let builder = Box::new(TelemetryWorkerBuilder::new(
                "h".into(),
                "s".into(),
                "rust".into(),
                "1".into(),
                "1".into(),
            ));
            let (handle, worker) = spawn_worker(builder, false);
            let outer = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let res = outer.block_on(async move { ddog_telemetry_worker_drop(worker) });
            assert!(matches!(res, MaybeError::Some(_)));
            ddog_telemetry_handle_drop(handle);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_spawn_null_runtime_errors() {
        unsafe {
            let builder = Box::new(TelemetryWorkerBuilder::new(
                "h".into(),
                "s".into(),
                "rust".into(),
                "1".into(),
                "1".into(),
            ));
            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            let mut worker: MaybeUninit<Box<TelemetryWorkerRuntimeHandle>> = MaybeUninit::uninit();
            let res = ddog_telemetry_builder_spawn(
                builder,
                std::ptr::null(),
                false,
                NonNull::new(&mut handle).unwrap().cast(),
                NonNull::new(&mut worker).unwrap().cast(),
            );
            assert!(matches!(res, MaybeError::Some(_)));
        }
    }

    /// Test-side alias: matches the FFI surface, which pins `C` to
    /// [`NativeCapabilities`].
    type TelemetryWorkerHandle = libdd_telemetry::worker::TelemetryWorkerHandle<NativeCapabilities>;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_set_builder_str_param() {
        unsafe {
            let mut builder: MaybeUninit<Box<TelemetryWorkerBuilder>> = MaybeUninit::uninit();
            assert_eq!(
                ddog_telemetry_builder_instantiate(
                    NonNull::new(&mut builder).unwrap().cast(),
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None
            );
            let mut builder = builder.assume_init();

            assert_eq!(
                ddog_telemetry_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("runtime_id"),
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_telemetry_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("application.runtime_name"),
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_telemetry_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("host.kernel_version"),
                    ffi::CharSlice::from("ダタドグ")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.host.kernel_version.as_deref(), Some("ダタドグ"));

            assert!(
                ddog_telemetry_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("doesnt exist"),
                    ffi::CharSlice::from("abc")
                )
                .to_std()
                .is_none(),
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_set_builder_enum_param() {
        let mut builder: MaybeUninit<Box<TelemetryWorkerBuilder>> = MaybeUninit::uninit();
        unsafe {
            assert_eq!(
                ddog_telemetry_builder_instantiate(
                    NonNull::new(&mut builder).unwrap().cast(),
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None,
            );
            let mut builder = builder.assume_init();

            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::RuntimeId,
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::SessionId,
                    ffi::CharSlice::from("sess-1")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.config.session_id.as_deref(), Some("sess-1"));
            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::RootSessionId,
                    ffi::CharSlice::from("root-9")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.config.root_session_id.as_deref(), Some("root-9"));
            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::ParentSessionId,
                    ffi::CharSlice::from("parent-2")
                ),
                MaybeError::None,
            );
            assert_eq!(
                builder.config.parent_session_id.as_deref(),
                Some("parent-2")
            );

            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::ApplicationRuntimeName,
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_telemetry_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::HostKernelVersion,
                    ffi::CharSlice::from("ダタドグ")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.host.kernel_version.as_deref(), Some("ダタドグ"));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_run() {
        unsafe {
            let mut builder: MaybeUninit<Box<TelemetryWorkerBuilder>> = MaybeUninit::uninit();
            assert_eq!(
                ddog_telemetry_builder_instantiate(
                    NonNull::new(&mut builder).unwrap().cast(),
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None
            );
            let mut builder = builder.assume_init();

            let f = tempfile::NamedTempFile::new().unwrap();
            let url = format!("file://{}", f.path().as_os_str().to_str().unwrap());
            assert_eq!(
                ddog_telemetry_builder_with_endpoint_config_endpoint(
                    &mut builder,
                    ffi::CharSlice::from(url.as_str()),
                    ffi::CharSlice::from(""),
                    0,
                    ffi::CharSlice::from(""),
                    false,
                ),
                MaybeError::None
            );
            ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
                &mut builder,
                true,
            )
            .unwrap_none();

            let (handle, worker) = spawn_worker(builder, false);

            ddog_telemetry_handle_start(&handle).unwrap_none();
            ddog_telemetry_handle_stop(&handle).unwrap_none();
            stop_worker(handle, worker);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_metrics_worker_run() {
        unsafe {
            let mut builder: MaybeUninit<Box<TelemetryWorkerBuilder>> = MaybeUninit::uninit();
            assert_eq!(
                ddog_telemetry_builder_instantiate(
                    NonNull::new(&mut builder).unwrap().cast(),
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None
            );
            let mut builder = builder.assume_init();

            let f = tempfile::NamedTempFile::new().unwrap();
            let url = format!("file://{}", f.path().as_os_str().to_str().unwrap());
            assert_eq!(
                ddog_telemetry_builder_with_endpoint_config_endpoint(
                    &mut builder,
                    ffi::CharSlice::from(url.as_str()),
                    ffi::CharSlice::from(""),
                    0,
                    ffi::CharSlice::from(""),
                    false,
                ),
                MaybeError::None
            );
            ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
                &mut builder,
                true,
            )
            .unwrap_none();

            let (handle, worker) = spawn_worker(builder, true);

            assert!(matches!(
                ddog_telemetry_handle_start(&handle),
                MaybeError::None
            ));

            let mut tags = ddog_Vec_Tag_new();
            assert!(matches!(
                ddog_Vec_Tag_push(
                    &mut tags,
                    ffi::CharSlice::from("foo"),
                    ffi::CharSlice::from("bar"),
                ),
                PushTagResult::Ok
            ));

            let context_key = ddog_telemetry_handle_register_metric_context(
                &handle,
                ffi::CharSlice::from("test_metric"),
                MetricType::Count,
                tags,
                true,
                MetricNamespace::Apm,
            );
            ddog_telemetry_handle_add_point(&handle, &context_key, 1.0).unwrap_none();

            assert_eq!(ddog_telemetry_handle_stop(&handle), MaybeError::None);
            stop_worker(handle, worker);
        }
    }
}
