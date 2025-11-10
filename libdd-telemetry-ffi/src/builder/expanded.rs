// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use macros::*;
#[allow(clippy::redundant_closure_call)]
#[allow(clippy::missing_safety_doc)]
#[allow(unused_parens)]
#[allow(clippy::double_parens)]
mod macros {
    use ffi::slice::AsBytes;
    use libdd_common_ffi as ffi;
    use libdd_telemetry::worker::TelemetryWorkerBuilder;
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_service_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.service_version =
            match (|s: ffi::CharSlice| -> Result<_, String> {
                Ok(Some(s.to_utf8_lossy().into_owned()))
            })(param)
            {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                        ({
                            let res = std::fmt::format(format_args!("{e:?}"));
                            res
                        }),
                    ));
                }
            };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_env(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.env = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_name(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_name =
            match (|s: ffi::CharSlice| -> Result<_, String> {
                Ok(Some(s.to_utf8_lossy().into_owned()))
            })(param)
            {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                        ({
                            let res = std::fmt::format(format_args!("{e:?}"));
                            res
                        }),
                    ));
                }
            };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_version =
            match (|s: ffi::CharSlice| -> Result<_, String> {
                Ok(Some(s.to_utf8_lossy().into_owned()))
            })(param)
            {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                        ({
                            let res = std::fmt::format(format_args!("{e:?}"));
                            res
                        }),
                    ));
                }
            };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_application_runtime_patches(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.application.runtime_patches =
            match (|s: ffi::CharSlice| -> Result<_, String> {
                Ok(Some(s.to_utf8_lossy().into_owned()))
            })(param)
            {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                        ({
                            let res = std::fmt::format(format_args!("{e:?}"));
                            res
                        }),
                    ));
                }
            };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_container_id(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.container_id = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_os(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.os = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_name(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_name = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_release(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_release = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_host_kernel_version(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.host.kernel_version = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_str_runtime_id(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> ffi::MaybeError {
        telemetry_builder.runtime_id = match (|s: ffi::CharSlice| -> Result<_, String> {
            Ok(Some(s.to_utf8_lossy().into_owned()))
        })(param)
        {
            Ok(o) => o,
            Err(e) => {
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
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
                telemetry_builder.application.service_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            ApplicationEnv => {
                telemetry_builder.application.env =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            ApplicationRuntimeName => {
                telemetry_builder.application.runtime_name =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            ApplicationRuntimeVersion => {
                telemetry_builder.application.runtime_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            ApplicationRuntimePatches => {
                telemetry_builder.application.runtime_patches =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            HostContainerId => {
                telemetry_builder.host.container_id =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            HostOs => {
                telemetry_builder.host.os = match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(Some(s.to_utf8_lossy().into_owned()))
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                            ({
                                let res = std::fmt::format(format_args!("{e:?}"));
                                res
                            }),
                        ));
                    }
                };
            }
            HostKernelName => {
                telemetry_builder.host.kernel_name =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            HostKernelRelease => {
                telemetry_builder.host.kernel_release =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            HostKernelVersion => {
                telemetry_builder.host.kernel_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            RuntimeId => {
                telemetry_builder.runtime_id = match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(Some(s.to_utf8_lossy().into_owned()))
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                            ({
                                let res = std::fmt::format(format_args!("{e:?}"));
                                res
                            }),
                        ));
                    }
                };
            }
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
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
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        match property {
            "application.service_version" => {
                telemetry_builder.application.service_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "application.env" => {
                telemetry_builder.application.env =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "application.runtime_name" => {
                telemetry_builder.application.runtime_name =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "application.runtime_version" => {
                telemetry_builder.application.runtime_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "application.runtime_patches" => {
                telemetry_builder.application.runtime_patches =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "host.container_id" => {
                telemetry_builder.host.container_id =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "host.os" => {
                telemetry_builder.host.os = match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(Some(s.to_utf8_lossy().into_owned()))
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                            ({
                                let res = std::fmt::format(format_args!("{e:?}"));
                                res
                            }),
                        ));
                    }
                };
            }
            "host.kernel_name" => {
                telemetry_builder.host.kernel_name =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "host.kernel_release" => {
                telemetry_builder.host.kernel_release =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "host.kernel_version" => {
                telemetry_builder.host.kernel_version =
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(Some(s.to_utf8_lossy().into_owned()))
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            "runtime_id" => {
                telemetry_builder.runtime_id = match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(Some(s.to_utf8_lossy().into_owned()))
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                            ({
                                let res = std::fmt::format(format_args!("{e:?}"));
                                res
                            }),
                        ));
                    }
                };
            }
            _ => return ffi::MaybeError::None,
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
    pub unsafe extern "C" fn ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(
        telemetry_builder: &mut TelemetryWorkerBuilder,
        param: bool,
    ) -> ffi::MaybeError {
        telemetry_builder.config.telemetry_debug_logging_enabled =
            match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                Ok(o) => o,
                Err(e) => {
                    return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                        ({
                            let res = std::fmt::format(format_args!("{e:?}"));
                            res
                        }),
                    ));
                }
            };
        ffi::MaybeError::None
    }
    #[repr(C)]
    #[allow(dead_code)]
    pub enum TelemetryWorkerBuilderBoolProperty {
        ConfigTelemetryDebugLoggingEnabled,
    }
    #[no_mangle]
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
                    match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
        }
        ffi::MaybeError::None
    }
    #[no_mangle]
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
                return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                    ({
                        let res = std::fmt::format(format_args!("{e:?}"));
                        res
                    }),
                ));
            }
        };
        match property {
            "config.telemetry_debug_logging_enabled" => {
                telemetry_builder.config.telemetry_debug_logging_enabled =
                    match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return ffi::MaybeError::Some(libdd_common_ffi::Error::from(
                                ({
                                    let res = std::fmt::format(format_args!("{e:?}"));
                                    res
                                }),
                            ));
                        }
                    };
            }
            _ => return ffi::MaybeError::None,
        }
        ffi::MaybeError::None
    }
}
