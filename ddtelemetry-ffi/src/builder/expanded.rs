// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use macros::*;
mod macros {
    use ddcommon_ffi as ffi;
    use ddcommon_net1::Endpoint;
    use ddtelemetry::worker::TelemetryWorkerBuilder;
    use ffi::slice::AsBytes;
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_service_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.service_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_env(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.env =
            Some(
                match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(s.to_utf8_lossy().into_owned())
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }));
                    }
                },
            );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_name(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_name = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_patches(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_patches = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_container_id(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.container_id = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_os(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.os = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_name(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_name = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_release(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_release = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_runtime_id(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.runtime_id = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[repr(C)]
    #[allow(dead_code)]
    pub enum TelemetryWorkerBuilderStrProperty {
        ApplicationServiceVersion,
        ApplicationEnv,
        ApplicationRuntimeName,
        ApplicationRuntimeVersion,
        ApplicationRuntimePatches,
        HostContainerId,
        HostOs,
        HostKernelName,
        HostKernelRelease,
        HostKernelVersion,
        RuntimeId,
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * application.service_version

     * application.env

     * application.runtime_name

     * application.runtime_version

     * application.runtime_patches

     * host.container_id

     * host.os

     * host.kernel_name

     * host.kernel_release

     * host.kernel_version

     * runtime_id

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_property_str(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderStrProperty,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        use TelemetryWorkerBuilderStrProperty::*;
        match property {
            ApplicationServiceVersion => {
                telemetry_builder.application.service_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            ApplicationEnv => {
                telemetry_builder.application.env = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            ApplicationRuntimeName => {
                telemetry_builder.application.runtime_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            ApplicationRuntimeVersion => {
                telemetry_builder.application.runtime_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            ApplicationRuntimePatches => {
                telemetry_builder.application.runtime_patches = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            HostContainerId => {
                telemetry_builder.host.container_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            HostOs => {
                telemetry_builder.host.os =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }));
                            }
                        },
                    );
            }
            HostKernelName => {
                telemetry_builder.host.kernel_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            HostKernelRelease => {
                telemetry_builder.host.kernel_release = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            HostKernelVersion => {
                telemetry_builder.host.kernel_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            RuntimeId => {
                telemetry_builder.runtime_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * application.service_version

     * application.env

     * application.runtime_name

     * application.runtime_version

     * application.runtime_patches

     * host.container_id

     * host.os

     * host.kernel_name

     * host.kernel_release

     * host.kernel_version

     * runtime_id

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_named_property(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                    let res = std::fmt::format(format_args!("{0:?}", e));
                    res
                }));
            }
        };
        match property {
            "application.service_version" => {
                telemetry_builder.application.service_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "application.env" => {
                telemetry_builder.application.env = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "application.runtime_name" => {
                telemetry_builder.application.runtime_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "application.runtime_version" => {
                telemetry_builder.application.runtime_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "application.runtime_patches" => {
                telemetry_builder.application.runtime_patches = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "host.container_id" => {
                telemetry_builder.host.container_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "host.os" => {
                telemetry_builder.host.os =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }));
                            }
                        },
                    );
            }
            "host.kernel_name" => {
                telemetry_builder.host.kernel_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "host.kernel_release" => {
                telemetry_builder.host.kernel_release = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "host.kernel_version" => {
                telemetry_builder.host.kernel_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            "runtime_id" => {
                telemetry_builder.runtime_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            _ => return ffi::MaybeError::None,
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: bool,
    ) -> ffi::MaybeError {
        telemetry_builder.config.telemetry_debug_logging_enabled =
            Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            });
        ffi::MaybeError::None
    }
    #[repr(C)]
    #[allow(dead_code)]
    pub enum TelemetryWorkerBuilderBoolProperty {
        ConfigTelemetryDebugLoggingEnabled,
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.telemetry_debug_logging_enabled

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_property_bool(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderBoolProperty,
        param: bool,
    ) -> ffi::MaybeError {
        use TelemetryWorkerBuilderBoolProperty::*;
        match property {
            ConfigTelemetryDebugLoggingEnabled => {
                telemetry_builder.config.telemetry_debug_logging_enabled =
                    Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    });
            }
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.telemetry_debug_logging_enabled

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_bool_named_property(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: bool,
    ) -> ffi::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                    let res = std::fmt::format(format_args!("{0:?}", e));
                    res
                }));
            }
        };
        match property {
            "config.telemetry_debug_logging_enabled" => {
                telemetry_builder.config.telemetry_debug_logging_enabled =
                    Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    });
            }
            _ => return ffi::MaybeError::None,
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_config_endpoint(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: &Endpoint,
    ) -> ffi::MaybeError {
        telemetry_builder.config.endpoint = Some(
            match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }));
                }
            },
        );
        ffi::MaybeError::None
    }
    #[repr(C)]
    #[allow(dead_code)]
    pub enum TelemetryWorkerBuilderEndpointProperty {
        ConfigEndpoint,
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.endpoint

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_property_endpoint(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderEndpointProperty,
        param: &Endpoint,
    ) -> ffi::MaybeError {
        use TelemetryWorkerBuilderEndpointProperty::*;
        match property {
            ConfigEndpoint => {
                telemetry_builder.config.endpoint = Some(
                    match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.endpoint

    */
    pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_named_property(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: &Endpoint,
    ) -> ffi::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                    let res = std::fmt::format(format_args!("{0:?}", e));
                    res
                }));
            }
        };
        match property {
            "config.endpoint" => {
                telemetry_builder.config.endpoint = Some(
                    match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(ddcommon_ffi::Error::from({
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }));
                        }
                    },
                );
            }
            _ => return ffi::MaybeError::None,
        }
        ffi::MaybeError::None
    }
}
