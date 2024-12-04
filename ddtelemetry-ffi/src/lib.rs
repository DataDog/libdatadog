// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
                #[allow(clippy::redundant_closure_call)]
                #[allow(clippy::missing_safety_doc)]
                pub unsafe extern "C" fn [<ddog_ $object_name _with_ $property_type_name_snakecase _ $path $(_ $path_rest)* >](
                    $object_name: &mut $object_ty,
                    param: $property_type,
                ) -> ffi::MaybeError {
                    $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
                    ffi::MaybeError::None
                }
            )+

            #[repr(C)]
            #[allow(dead_code)]
            pub enum [<$object_ty $property_type_name_camel_case Property >] {
                $([< $path:camel $($path_rest:camel)* >],)+
            }

            #[no_mangle]
            #[allow(clippy::redundant_closure_call)]
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
                            $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
                        }
                    )+
                }
                ffi::MaybeError::None
            }

            #[no_mangle]
            #[allow(clippy::redundant_closure_call)]
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
                            $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
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
            Err(e) => return ffi::MaybeError::Some(ddcommon_ffi::Error::from(format!("{:?}", e))),
        }
    };
}

#[allow(unused_imports)]
pub(crate) use c_setters;

#[cfg(test)]
mod tests {
    use crate::{builder::*, worker_handle::*};
    use ddcommon_ffi as ffi;
    use ddcommon_net1::Endpoint;
    use ddtelemetry::{
        data::metrics::{MetricNamespace, MetricType},
        worker::{TelemetryWorkerBuilder, TelemetryWorkerHandle},
    };
    use ffi::tags::{ddog_Vec_Tag_new, ddog_Vec_Tag_push, PushTagResult};
    use ffi::MaybeError;
    use std::{mem::MaybeUninit, ptr::NonNull};

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
            );

            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            ddog_telemetry_builder_run(builder, NonNull::new(&mut handle).unwrap().cast());
            let handle = handle.assume_init();

            ddog_telemetry_handle_start(&handle);
            ddog_telemetry_handle_stop(&handle);
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
            );

            let mut handle: MaybeUninit<Box<TelemetryWorkerHandle>> = MaybeUninit::uninit();
            ddog_telemetry_builder_run_metric_logs(
                builder,
                NonNull::new(&mut handle).unwrap().cast(),
            );
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
            ddog_telemetry_handle_add_point(&handle, &context_key, 1.0);

            assert_eq!(ddog_telemetry_handle_stop(&handle), MaybeError::None);
            ddog_telemetry_handle_wait_for_shutdown(handle);
        }
    }
}
