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
                #[no_mangle]
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

            #[no_mangle]
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

            #[no_mangle]
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

#[allow(unused_imports)]
pub(crate) use c_setters;

#[cfg(test)]
mod tests {
    use crate::{builder::*, worker_handle::*};
    use ffi::tags::{ddog_Vec_Tag_new, ddog_Vec_Tag_push, PushTagResult};
    use ffi::MaybeError;
    use libdd_common::Endpoint;
    use libdd_common_ffi as ffi;
    use libdd_telemetry::{
        data::metrics::{MetricNamespace, MetricType},
        worker::{TelemetryWorkerBuilder, TelemetryWorkerHandle},
    };
    use std::{mem::MaybeUninit, ptr::NonNull};

    /// Spins up a worker backed by a file:// endpoint, returns (handle, temp_file).
    /// The caller is responsible for stopping the worker and reading the file.
    unsafe fn start_file_backed_worker() -> (Box<TelemetryWorkerHandle>, tempfile::NamedTempFile) {
        let mut builder: MaybeUninit<Box<TelemetryWorkerBuilder>> = MaybeUninit::uninit();
        ddog_telemetry_builder_instantiate(
            NonNull::new(&mut builder).unwrap().cast(),
            ffi::CharSlice::from("test-service"),
            ffi::CharSlice::from("rust"),
            ffi::CharSlice::from("1.0"),
            ffi::CharSlice::from("0.0.1"),
        )
        .unwrap_none();
        let mut builder = builder.assume_init();

        let f = tempfile::NamedTempFile::new().unwrap();
        ddog_telemetry_builder_with_endpoint_config_endpoint(
            &mut builder,
            &Endpoint::from_slice(&format!("file://{}", f.path().to_str().unwrap())),
        )
        .unwrap_none();

        let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
        ddog_telemetry_builder_run(builder, NonNull::new(&mut handle).unwrap().cast())
            .unwrap_none();
        let handle = handle.assume_init();
        ddog_telemetry_handle_start(&handle).unwrap_none();
        (handle, f)
    }

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

            assert!(ddog_telemetry_builder_with_str_named_property(
                &mut builder,
                ffi::CharSlice::from("doesnt exist"),
                ffi::CharSlice::from("abc")
            )
            .to_std()
            .is_none(),);
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
            assert_eq!(
                ddog_telemetry_builder_with_endpoint_config_endpoint(
                    &mut builder,
                    &Endpoint::from_slice(&format!(
                        "file://{}",
                        f.path().as_os_str().to_str().unwrap()
                    )),
                ),
                MaybeError::None
            );
            ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
                &mut builder,
                true,
            )
            .unwrap_none();

            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            ddog_telemetry_builder_run(builder, NonNull::new(&mut handle).unwrap().cast())
                .unwrap_none();
            let handle = handle.assume_init();

            ddog_telemetry_handle_start(&handle).unwrap_none();
            ddog_telemetry_handle_stop(&handle).unwrap_none();
            ddog_telemetry_handle_wait_for_shutdown(handle);
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
            assert_eq!(
                ddog_telemetry_builder_with_endpoint_config_endpoint(
                    &mut builder,
                    &Endpoint::from_slice(&format!(
                        "file://{}",
                        f.path().as_os_str().to_str().unwrap()
                    )),
                ),
                MaybeError::None
            );
            ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
                &mut builder,
                true,
            )
            .unwrap_none();

            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            ddog_telemetry_builder_run_metric_logs(
                builder,
                NonNull::new(&mut handle).unwrap().cast(),
            )
            .unwrap_none();
            let handle = handle.assume_init();

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
            ddog_telemetry_handle_wait_for_shutdown(handle);
        }
    }

    /// Finds all sub-payloads with the given request_type in a file endpoint output.
    /// Handles both top-level payloads and those nested inside message-batch.
    fn find_sub_payloads(output: &str, request_type: &str) -> Vec<serde_json::Value> {
        let mut results = Vec::new();
        for line in output.lines() {
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            collect_payloads_recursive(&parsed, request_type, &mut results);
        }
        results
    }

    fn collect_payloads_recursive(
        value: &serde_json::Value,
        target_type: &str,
        results: &mut Vec<serde_json::Value>,
    ) {
        if value["request_type"].as_str() == Some(target_type) {
            results.push(value.clone());
        }
        if value["request_type"].as_str() == Some("message-batch") {
            if let Some(batch) = value["payload"].as_array() {
                for item in batch {
                    collect_payloads_recursive(item, target_type, results);
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_dependency_preserves_version() {
        unsafe {
            let (handle, f) = start_file_backed_worker();

            ddog_telemetry_handle_add_dependency(
                &handle,
                ffi::CharSlice::from("my-test-crate"),
                ffi::CharSlice::from("1.2.3"),
            )
            .unwrap_none();

            ddog_telemetry_handle_add_dependency(
                &handle,
                ffi::CharSlice::from("versionless-crate"),
                ffi::CharSlice::from(""),
            )
            .unwrap_none();

            ddog_telemetry_handle_stop(&handle).unwrap_none();
            ddog_telemetry_handle_wait_for_shutdown(handle);

            let output = std::fs::read_to_string(f.path()).unwrap();
            let dep_payloads = find_sub_payloads(&output, "app-dependencies-loaded");
            assert!(
                !dep_payloads.is_empty(),
                "expected at least one app-dependencies-loaded payload in output:\n{output}"
            );

            let deps: Vec<&serde_json::Value> = dep_payloads
                .iter()
                .filter_map(|p| p["payload"]["dependencies"].as_array())
                .flatten()
                .collect();

            let versioned = deps
                .iter()
                .find(|d| d["name"] == "my-test-crate")
                .expect("expected my-test-crate in dependencies");
            assert_eq!(
                versioned["version"].as_str(),
                Some("1.2.3"),
                "Non-empty version must be preserved through the FFI layer"
            );

            let versionless = deps
                .iter()
                .find(|d| d["name"] == "versionless-crate")
                .expect("expected versionless-crate in dependencies");
            assert!(
                versionless["version"].is_null(),
                "Empty version string should map to null/None, got {:?}",
                versionless["version"]
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_integration_preserves_version() {
        unsafe {
            let (handle, f) = start_file_backed_worker();

            ddog_telemetry_handle_add_integration(
                &handle,
                ffi::CharSlice::from("http-framework"),
                ffi::CharSlice::from("2.0.0"),
                true,
                ffi::Option::<bool>::None,
                ffi::Option::<bool>::None,
            )
            .unwrap_none();

            ddog_telemetry_handle_add_integration(
                &handle,
                ffi::CharSlice::from("versionless-integration"),
                ffi::CharSlice::from(""),
                true,
                ffi::Option::<bool>::None,
                ffi::Option::<bool>::None,
            )
            .unwrap_none();

            ddog_telemetry_handle_stop(&handle).unwrap_none();
            ddog_telemetry_handle_wait_for_shutdown(handle);

            let output = std::fs::read_to_string(f.path()).unwrap();
            let int_payloads = find_sub_payloads(&output, "app-integrations-change");
            assert!(
                !int_payloads.is_empty(),
                "expected at least one app-integrations-change payload in output:\n{output}"
            );

            let integrations: Vec<&serde_json::Value> = int_payloads
                .iter()
                .filter_map(|p| p["payload"]["integrations"].as_array())
                .flatten()
                .collect();

            let versioned = integrations
                .iter()
                .find(|i| i["name"] == "http-framework")
                .expect("expected http-framework in integrations");
            assert_eq!(
                versioned["version"].as_str(),
                Some("2.0.0"),
                "Non-empty version must be preserved through the FFI layer"
            );

            let versionless = integrations
                .iter()
                .find(|i| i["name"] == "versionless-integration")
                .expect("expected versionless-integration in integrations");
            assert!(
                versionless["version"].is_null(),
                "Empty version string should map to null/None, got {:?}",
                versionless["version"]
            );
        }
    }
}
