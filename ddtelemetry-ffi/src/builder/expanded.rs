// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use macros::*;
mod macros {
    use ddcommon::Endpoint;
    use ddcommon_ffi as ffi;
    use ddtelemetry::worker::TelemetryWorkerBuilder;
    use ffi::slice::AsBytes;
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_application_service_version(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.application.service_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_application_env(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.application.env = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_application_runtime_name(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.application.runtime_name =
            Some(
                match (|s: ffi::CharSlice| -> Result<_, String> {
                    Ok(s.to_utf8_lossy().into_owned())
                })(param)
                {
                    Ok(o) => o,
                    Err(e) => {
                        return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                            {
                                let res = std::fmt::format(format_args!("{0:?}", e));
                                res
                            }
                            .into_bytes(),
                        ));
                    }
                },
            );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_application_runtime_version(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.application.runtime_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_application_runtime_patches(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.application.runtime_patches = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_host_container_id(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.host.container_id = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_host_os(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.host.os = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_host_kernel_name(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.host.kernel_name = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_host_kernel_release(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.host.kernel_release = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_host_kernel_version(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.host.kernel_version = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_str_runtime_id(
        builder: &mut TelemetryWorkerBuilder,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        builder.runtime_id = Some(
            match (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) })(
                param,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
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
    pub unsafe extern "C" fn ddog_builder_with_property_str(
        builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderStrProperty,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        use TelemetryWorkerBuilderStrProperty::*;
        match property {
            ApplicationServiceVersion => {
                builder.application.service_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            ApplicationEnv => {
                builder.application.env = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            ApplicationRuntimeName => {
                builder.application.runtime_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            ApplicationRuntimeVersion => {
                builder.application.runtime_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            ApplicationRuntimePatches => {
                builder.application.runtime_patches = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            HostContainerId => {
                builder.host.container_id =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                    {
                                        let res = std::fmt::format(format_args!("{0:?}", e));
                                        res
                                    }
                                    .into_bytes(),
                                ));
                            }
                        },
                    );
            }
            HostOs => {
                builder.host.os = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            HostKernelName => {
                builder.host.kernel_name =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                    {
                                        let res = std::fmt::format(format_args!("{0:?}", e));
                                        res
                                    }
                                    .into_bytes(),
                                ));
                            }
                        },
                    );
            }
            HostKernelRelease => {
                builder.host.kernel_release = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            HostKernelVersion => {
                builder.host.kernel_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            RuntimeId => {
                builder.runtime_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
        }
        crate::MaybeError::None
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
    pub unsafe extern "C" fn ddog_builder_with_str_named_property(
        builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: ffi::CharSlice,
    ) -> crate::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                    {
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }
                    .into_bytes(),
                ));
            }
        };
        match property {
            "application.service_version" => {
                builder.application.service_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "application.env" => {
                builder.application.env = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "application.runtime_name" => {
                builder.application.runtime_name = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "application.runtime_version" => {
                builder.application.runtime_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "application.runtime_patches" => {
                builder.application.runtime_patches = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "host.container_id" => {
                builder.host.container_id =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                    {
                                        let res = std::fmt::format(format_args!("{0:?}", e));
                                        res
                                    }
                                    .into_bytes(),
                                ));
                            }
                        },
                    );
            }
            "host.os" => {
                builder.host.os = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "host.kernel_name" => {
                builder.host.kernel_name =
                    Some(
                        match (|s: ffi::CharSlice| -> Result<_, String> {
                            Ok(s.to_utf8_lossy().into_owned())
                        })(param)
                        {
                            Ok(o) => o,
                            Err(e) => {
                                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                    {
                                        let res = std::fmt::format(format_args!("{0:?}", e));
                                        res
                                    }
                                    .into_bytes(),
                                ));
                            }
                        },
                    );
            }
            "host.kernel_release" => {
                builder.host.kernel_release = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "host.kernel_version" => {
                builder.host.kernel_version = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            "runtime_id" => {
                builder.runtime_id = Some(
                    match (|s: ffi::CharSlice| -> Result<_, String> {
                        Ok(s.to_utf8_lossy().into_owned())
                    })(param)
                    {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            _ => return crate::MaybeError::None,
        }
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_bool_config_telemetry_debug_logging_enabled(
        builder: &mut TelemetryWorkerBuilder,
        param: bool,
    ) -> crate::MaybeError {
        builder.config.telemetry_debug_logging_enabled =
            Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            });
        crate::MaybeError::None
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
    pub unsafe extern "C" fn ddog_builder_with_property_bool(
        builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderBoolProperty,
        param: bool,
    ) -> crate::MaybeError {
        use TelemetryWorkerBuilderBoolProperty::*;
        match property {
            ConfigTelemetryDebugLoggingEnabled => {
                builder.config.telemetry_debug_logging_enabled =
                    Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    });
            }
        }
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.telemetry_debug_logging_enabled

    */
    pub unsafe extern "C" fn ddog_builder_with_bool_named_property(
        builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: bool,
    ) -> crate::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                    {
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }
                    .into_bytes(),
                ));
            }
        };
        match property {
            "config.telemetry_debug_logging_enabled" => {
                builder.config.telemetry_debug_logging_enabled =
                    Some(match (|b: bool| -> Result<_, String> { Ok(b) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    });
            }
            _ => return crate::MaybeError::None,
        }
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    pub unsafe extern "C" fn ddog_builder_with_endpoint_config_endpoint(
        builder: &mut TelemetryWorkerBuilder,
        param: &Endpoint,
    ) -> crate::MaybeError {
        builder.config.endpoint = Some(
            match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                Ok(o) => o,
                Err(e) => {
                    return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                        {
                            let res = std::fmt::format(format_args!("{0:?}", e));
                            res
                        }
                        .into_bytes(),
                    ));
                }
            },
        );
        crate::MaybeError::None
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
    pub unsafe extern "C" fn ddog_builder_with_property_endpoint(
        builder: &mut TelemetryWorkerBuilder,
        property: TelemetryWorkerBuilderEndpointProperty,
        param: &Endpoint,
    ) -> crate::MaybeError {
        use TelemetryWorkerBuilderEndpointProperty::*;
        match property {
            ConfigEndpoint => {
                builder.config.endpoint = Some(
                    match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
        }
        crate::MaybeError::None
    }
    #[no_mangle]
    #[allow(clippy::redundant_closure_call)]
    #[allow(clippy::missing_safety_doc)]
    /**
     Sets a property from it's string value.

     Available properties:

     * config.endpoint

    */
    pub unsafe extern "C" fn ddog_builder_with_endpoint_named_property(
        builder: &mut TelemetryWorkerBuilder,
        property: ffi::CharSlice,
        param: &Endpoint,
    ) -> crate::MaybeError {
        let property = match property.try_to_utf8() {
            Ok(o) => o,
            Err(e) => {
                return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                    {
                        let res = std::fmt::format(format_args!("{0:?}", e));
                        res
                    }
                    .into_bytes(),
                ));
            }
        };
        match property {
            "config.endpoint" => {
                builder.config.endpoint = Some(
                    match (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) })(param) {
                        Ok(o) => o,
                        Err(e) => {
                            return crate::MaybeError::Some(ddcommon_ffi::Vec::from(
                                {
                                    let res = std::fmt::format(format_args!("{0:?}", e));
                                    res
                                }
                                .into_bytes(),
                            ));
                        }
                    },
                );
            }
            _ => return crate::MaybeError::None,
        }
        crate::MaybeError::None
    }
}
