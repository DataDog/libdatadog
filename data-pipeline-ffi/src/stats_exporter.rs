// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{ptr::NonNull, time};

use crate::try_c;
use data_pipeline::stats_exporter::{
    blocking::StatsExporter, endpoint_from_agent_url, Configuration, LibraryMetadata, SpanStats,
};
use ddcommon::{parse_uri, tag::Tag};
use ddcommon_ffi as ffi;
use ffi::slice::AsBytes;

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_new(
    hostname: ffi::CharSlice,
    env: ffi::CharSlice,
    version: ffi::CharSlice,
    lang: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
    runtime_id: ffi::CharSlice,
    service: ffi::CharSlice,
    container_id: ffi::CharSlice,
    git_commit_sha: ffi::CharSlice,
    tags: ffi::Vec<Tag>,

    agent_url: ffi::CharSlice,
    stats_computation_interval_seconds: u64,
    // No timeout if zero
    request_timeout_ms: u64,

    out_exporter: NonNull<Box<StatsExporter>>,
) -> ffi::Option<ffi::Error> {
    let endpoint =
        try_c!(parse_uri(agent_url.to_utf8_lossy().as_ref()).and_then(endpoint_from_agent_url));

    out_exporter
        .as_ptr()
        .write(Box::new(try_c!(StatsExporter::new(
            LibraryMetadata {
                hostname: hostname.to_utf8_lossy().into_owned(),
                env: env.to_utf8_lossy().into_owned(),
                version: version.to_utf8_lossy().into_owned(),
                lang: lang.to_utf8_lossy().into_owned(),
                tracer_version: tracer_version.to_utf8_lossy().into_owned(),
                runtime_id: runtime_id.to_utf8_lossy().into_owned(),
                service: service.to_utf8_lossy().into_owned(),
                container_id: container_id.to_utf8_lossy().into_owned(),
                git_commit_sha: git_commit_sha.to_utf8_lossy().into_owned(),
                tags: tags.into(),
            },
            Configuration {
                endpoint,
                stats_computation_interval: time::Duration::from_secs(
                    stats_computation_interval_seconds
                ),
                request_timeout: if request_timeout_ms != 0 {
                    Some(time::Duration::from_millis(request_timeout_ms))
                } else {
                    None
                }
            }
        ))));
    ffi::Option::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_insert_span_data(
    exporter: &StatsExporter,
    resource_name: ffi::CharSlice,
    service_name: ffi::CharSlice,
    operation_name: ffi::CharSlice,
    span_type: ffi::CharSlice,
    http_status_code: u32,
    is_synthetics_request: bool,
    is_top_level: bool,
    is_error: bool,
    duration: u64,
) {
    exporter.insert(SpanStats {
        resource_name: resource_name.to_utf8_lossy().into_owned(),
        service_name: service_name.to_utf8_lossy().into_owned(),
        operation_name: operation_name.to_utf8_lossy().into_owned(),
        span_type: span_type.to_utf8_lossy().into_owned(),
        http_status_code,
        is_synthetics_request,
        is_top_level,
        is_error,
        duration,
    })
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_send(
    exporter: &StatsExporter,
) -> ffi::Option<ffi::Error> {
    try_c!(exporter.send());
    ffi::Option::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_drop(_: Box<StatsExporter>) {}
