// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{ptr::NonNull, time};

use data_pipeline::stats_exporter::{
    blocking::StatsExporter, stats_url_from_agent_url, Configuration, LibraryMetadata, SpanStats,
};
use ddcommon::{parse_uri, tag::Tag, Endpoint};
use ddcommon_ffi as ffi;
use ffi::slice::AsBytes;

/// Create a new StatExporter instance.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporter instance in.
/// * `url` - The URL of the Datadog Agent to communicate with.
/// * `hostname` - The tracer hostname
/// * `env` - env tag, used for aggregation
/// * `version` - version tag, used for aggregation
/// * `lang` - The language of the client library.
/// * `tracer_version` - The version of the client library.
/// * `runtime_id` - Id used by the agent to identify uniquely a source
/// * `service` - service name
/// * `git_commit_sha` - A git commit sha set through source code integration
/// * `stats_computation_interval_seconds` - the size of stats buckets in seconds
/// * `request_timeout_ms` - request timeout in ms, no temout if zero
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_new(
    out_handle: NonNull<Box<StatsExporter>>,
    url: ffi::CharSlice,
    hostname: ffi::CharSlice,
    env: ffi::CharSlice,
    version: ffi::CharSlice,
    lang: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
    runtime_id: ffi::CharSlice,
    service: ffi::CharSlice,
    git_commit_sha: ffi::CharSlice,
    tags: ffi::Vec<Tag>,
    stats_computation_interval_seconds: u64,
    request_timeout_ms: u64,
) -> ffi::Option<ffi::Error> {
    let url =
        ffi::try_c!(parse_uri(url.to_utf8_lossy().as_ref()).and_then(stats_url_from_agent_url));

    let exporter = StatsExporter::new(
        LibraryMetadata {
            hostname: hostname.to_utf8_lossy().into_owned(),
            env: env.to_utf8_lossy().into_owned(),
            version: version.to_utf8_lossy().into_owned(),
            lang: lang.to_utf8_lossy().into_owned(),
            tracer_version: tracer_version.to_utf8_lossy().into_owned(),
            runtime_id: runtime_id.to_utf8_lossy().into_owned(),
            service: service.to_utf8_lossy().into_owned(),
            container_id: ddcommon::entity_id::get_container_id()
                .unwrap_or("")
                .to_owned(),
            git_commit_sha: git_commit_sha.to_utf8_lossy().into_owned(),
            tags: tags.into(),
        },
        Configuration {
            endpoint: Endpoint {
                url,
                ..Default::default()
            },
            buckets_duration: time::Duration::from_secs(stats_computation_interval_seconds),
            request_timeout: if request_timeout_ms != 0 {
                Some(time::Duration::from_millis(request_timeout_ms))
            } else {
                None
            },
        },
    );

    out_handle.as_ptr().write(Box::new(ffi::try_c!(exporter)));
    ffi::Option::None
}

/// Insert a span in a stats exporter and add it to the stats.
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
/// Flush stats form a stats exporter and send them to the agent
pub unsafe extern "C" fn ddog_stats_exporter_send(
    exporter: &StatsExporter,
) -> ffi::Option<ffi::Error> {
    ffi::try_c!(exporter.send());
    ffi::Option::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_stats_exporter_drop(_: Box<StatsExporter>) {}
