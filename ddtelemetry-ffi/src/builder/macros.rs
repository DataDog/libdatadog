// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// To regenerate expanded.rs run
// ```bash
// HEADER=$(cat ddtelemetry-ffi/src/builder/macros.rs | sed '/^$/q') EXPANDED=ddtelemetry-ffi/src/builder/expanded.rs &&
//   echo $HEADER '\n' > $EXPANDED && cargo expand -p ddtelemetry-ffi --no-default-features builder::macros |
//   sed 's/#\[cfg(not(feature = "expanded_builder_macros"))\]/pub use macros::*;/' |
//   sed 's/::alloc::fmt::format/std::fmt::format/' >> $EXPANDED && cargo fmt
// ```

use ddcommon_ffi as ffi;
use ddcommon_net1::Endpoint;
use ddtelemetry::worker::TelemetryWorkerBuilder;
use ffi::slice::AsBytes;

crate::c_setters!(
    object_name => telemetry_builder,
    object_type => TelemetryWorkerBuilder,
    property_type => ffi::CharSlice,
    property_type_name_snakecase => str,
    property_type_name_camel_case => Str,
    convert_fn => (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) }),
    SETTERS {
        application.service_version,
        application.env,
        application.runtime_name,
        application.runtime_version,
        application.runtime_patches,

        host.container_id,
        host.os,
        host.kernel_name,
        host.kernel_release,
        host.kernel_version,

        runtime_id
    }
);

crate::c_setters!(
    object_name => telemetry_builder,
    object_type => TelemetryWorkerBuilder,
    property_type => bool,
    property_type_name_snakecase => bool,
    property_type_name_camel_case => Bool,
    convert_fn => (|b: bool| -> Result<_, String> { Ok(b) }),
    SETTERS {
        config.telemetry_debug_logging_enabled,
    }
);

crate::c_setters!(
    object_name => telemetry_builder,
    object_type => TelemetryWorkerBuilder,
    property_type => &Endpoint,
    property_type_name_snakecase => endpoint,
    property_type_name_camel_case => Endpoint,
    convert_fn => (|e: &Endpoint| -> Result<_, String> { Ok(e.clone()) }),
    SETTERS {
        config.endpoint,
    }
);
