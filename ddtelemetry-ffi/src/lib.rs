// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon_ffi as ffi;

pub mod builder;
pub mod sidecar;
pub mod worker_handle;

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
                ) -> MaybeError {
                    $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
                    MaybeError::None
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
                " Sets a property from it's string value.\n\n",
                " # Available properties:\n\n",
                $(" * ", stringify!($path $(. $path_rest)*) , "\n\n",)+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_property_ $property_type_name_snakecase>](
                $object_name: &mut $object_ty,
                property: [<$object_ty $property_type_name_camel_case Property >],
                param: $property_type,
            ) -> MaybeError {
                use [<$object_ty $property_type_name_camel_case Property >] ::*;
                match property {
                    $(
                        [< $path:camel $($path_rest:camel)* >] => {
                            $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
                        }
                    )+
                }
                MaybeError::None
            }

            #[no_mangle]
            #[allow(clippy::redundant_closure_call)]
            #[allow(clippy::missing_safety_doc)]
            #[doc=concat!(
                " Sets a property from it's string value.\n\n",
                " # Available properties:\n\n",
                $(
                    " * ", stringify!($path $(. $path_rest)*) , "\n\n",
                )+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_ $property_type_name_snakecase _named_property>](
                $object_name: &mut $object_ty,
                property: ffi::CharSlice,
                param: $property_type,
            ) -> MaybeError {
                let property = crate::try_c!(property.try_to_utf8());
                match property {
                    $(
                        stringify!($path $(. $path_rest)*) => {
                            $object_name . $path $(.  $path_rest)* = Some(crate::try_c!($convert_fn (param)));
                        }
                    )+
                    // TODO this is an error
                    _ => return MaybeError::None,
                }
                MaybeError::None
            }
        }

    };
}

macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return MaybeError::Some(ddcommon_ffi::Vec::from(e.to_string().into_bytes())),
        }
    };
}

pub(crate) use c_setters;
pub(crate) use try_c;

pub type MaybeError = ffi::Option<ffi::Vec<u8>>;

#[no_mangle]
pub extern "C" fn ddog_MaybeError_drop(_: MaybeError) {}

#[cfg(test)]
mod test_c_ffi {
    use super::*;
    use crate::{builder::*, worker_handle::*};

    #[test]
    fn test_set_builder_mock_client_config() {
        unsafe {
            let mut builder = std::ptr::null_mut();
            ddog_builder_instantiate(
                &mut builder,
                ffi::CharSlice::from("service_name"),
                ffi::CharSlice::from("language_name"),
                ffi::CharSlice::from("language_version"),
                ffi::CharSlice::from("tracer_version"),
            );
            let mut builder = Box::from_raw(builder);
            ddog_builder_with_path_config_mock_client_file(
                &mut builder,
                ffi::CharSlice::from("/dev/null"),
            );

            assert_eq!(
                builder.config.mock_client_file.as_deref(),
                Some("/dev/null".as_ref())
            );
        }
    }

    #[test]
    fn test_set_builder_str_param() {
        let mut builder = std::ptr::null_mut();

        unsafe {
            assert_eq!(
                ddog_builder_instantiate(
                    &mut builder,
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None
            );
            assert!(!builder.is_null());
            let mut builder = Box::from_raw(builder);

            assert_eq!(
                ddog_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("runtime_id"),
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("application.runtime_name"),
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_builder_with_str_named_property(
                    &mut builder,
                    ffi::CharSlice::from("host.kernel_version"),
                    ffi::CharSlice::from("ダタドグ")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.host.kernel_version.as_deref(), Some("ダタドグ"));

            assert!(ddog_builder_with_str_named_property(
                &mut builder,
                ffi::CharSlice::from("doesnt exist"),
                ffi::CharSlice::from("abc")
            )
            .to_std()
            .is_none(),);
        }
    }

    #[test]
    fn test_set_builder_enum_param() {
        let mut builder = std::ptr::null_mut();

        unsafe {
            assert_eq!(
                ddog_builder_instantiate(
                    &mut builder,
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None,
            );
            assert!(!builder.is_null());
            let mut builder = Box::from_raw(builder);

            assert_eq!(
                ddog_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::RuntimeId,
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_builder_with_property_str(
                    &mut builder,
                    TelemetryWorkerBuilderStrProperty::ApplicationRuntimeName,
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_builder_with_property_str(
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
    fn test_worker_run() {
        unsafe {
            let mut builder = std::ptr::null_mut();
            ddog_builder_instantiate(
                &mut builder,
                ffi::CharSlice::from("service_name"),
                ffi::CharSlice::from("language_name"),
                ffi::CharSlice::from("language_version"),
                ffi::CharSlice::from("tracer_version"),
            );

            let mut builder = Box::from_raw(builder);

            let f = tempfile::NamedTempFile::new().unwrap();
            ddog_builder_with_path_config_mock_client_file(
                &mut builder,
                ffi::CharSlice::from(f.path().as_os_str().to_str().unwrap()),
            );
            ddog_builder_with_bool_config_telemetry_debug_logging_enabled(&mut builder, true);

            let mut handle = std::ptr::null_mut();
            ddog_builder_run(builder, &mut handle);
            let handle = Box::from_raw(handle);

            ddog_handle_start(&handle);
            ddog_handle_stop(&handle);
            ddog_handle_wait_for_shutdown(handle);
        }
    }
}
