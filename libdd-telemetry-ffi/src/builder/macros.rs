// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// To regenerate expanded.rs run
// ```bash
// HEADER=$(cat libdd-telemetry-ffi/src/builder/macros.rs | sed '/^$/q') EXPANDED=libdd-telemetry-ffi/src/builder/expanded.rs &&
//   echo $HEADER '\n' > $EXPANDED && cargo expand -p libdd-telemetry-ffi --no-default-features builder::macros |
//   sed 's/#\[cfg(not(feature = "expanded_builder_macros"))\]/pub use macros::*;/' |
//   sed 's/mod macros {/#[allow(clippy::redundant_closure_call)]\n#[allow(clippy::missing_safety_doc)]\n#[allow(unused_parens)]\n#[allow(clippy::double_parens)]\nmod macros {/' |
//   sed 's/::alloc::fmt::format/std::fmt::format/' |
//   sed 's/::alloc::__export::must_use//' |
//    >> $EXPANDED && cargo +nightly fmt
// ```

use ddcommon_ffi as ffi;
use ffi::slice::AsBytes;
use libdd_telemetry::worker::TelemetryWorkerBuilder;

crate::c_setters!(
    object_name => telemetry_builder,
    object_type => TelemetryWorkerBuilder,
    property_type => ffi::CharSlice,
    property_type_name_snakecase => str,
    property_type_name_camel_case => Str,
    convert_fn => (|s: ffi::CharSlice| -> Result<_, String> { Ok(Some(s.to_utf8_lossy().into_owned())) }),
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
