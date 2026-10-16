// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::super::{ddog_trace_exporter_free, ddog_trace_exporter_send};
use super::*;
use crate::error::ddog_trace_exporter_error_free;
use crate::response::ExporterResponse;
use httpmock::MockServer;
use httpmock::prelude::*;
use libdd_common_ffi::slice::ByteSlice;
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::span::v04::SpanSlice;
use std::mem::MaybeUninit;

type StringSetter = unsafe extern "C" fn(
    Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    CharSlice<'_>,
) -> Option<Box<ExporterError>>;

#[test]
#[allow(deprecated)]
fn legacy_boolean_setters_still_support_disabling() {
    use super::super as legacy;

    unsafe {
        let mut config = legacy::TraceExporterConfig::default();
        for enabled in [true, false] {
            assert_eq!(
                legacy::ddog_trace_exporter_config_set_compute_stats(Some(&mut config), enabled),
                None
            );
            assert_eq!(
                legacy::ddog_trace_exporter_config_set_client_computed_stats(
                    Some(&mut config),
                    enabled
                ),
                None
            );
            assert_eq!(
                legacy::ddog_trace_exporter_config_enable_health_metrics(
                    Some(&mut config),
                    enabled
                ),
                None
            );
            assert_eq!(config.compute_stats, enabled);
            assert_eq!(config.client_computed_stats, enabled);
            assert_eq!(config.health_metrics_enabled, enabled);
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
#[allow(deprecated)]
fn legacy_exporter_new_does_not_consume_config() {
    use super::super as legacy;

    unsafe {
        let mut config = Box::<legacy::TraceExporterConfig>::default();
        assert_eq!(
            legacy::ddog_trace_exporter_config_set_output_to_log(Some(&mut config), 0),
            None
        );
        for _ in 0..2 {
            let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
            assert_eq!(
                legacy::ddog_trace_exporter_new(NonNull::from(&mut exporter).cast(), Some(&config)),
                None
            );
            ddog_trace_exporter_free(exporter.assume_init());
        }
        legacy::ddog_trace_exporter_config_free(config);

        let mut config = Box::<legacy::TraceExporterConfig>::default();
        assert_eq!(
            legacy::ddog_trace_exporter_config_set_url(
                Some(&mut config),
                CharSlice::from("http://localhost:8126")
            ),
            None
        );
        assert_eq!(
            legacy::ddog_trace_exporter_config_set_agentless_endpoint(
                Some(&mut config),
                CharSlice::from("https://example.com/v1/input"),
                CharSlice::from("key")
            ),
            None
        );
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        let error =
            legacy::ddog_trace_exporter_new(NonNull::from(&mut exporter).cast(), Some(&config));
        assert!(error.is_some());
        ddog_trace_exporter_error_free(error);
        legacy::ddog_trace_exporter_config_free(config);
    }
}

#[test]
fn config_string_setters() {
    let setters: &[(StringSetter, &str)] = &[
        (ddog_trace_exporter_builder_set_url, "http://localhost:8126"),
        (ddog_trace_exporter_builder_set_tracer_version, "1.0"),
        (ddog_trace_exporter_builder_set_language, "lang"),
        (ddog_trace_exporter_builder_set_language_version, "0.1"),
        (
            ddog_trace_exporter_builder_set_language_interpreter,
            "interpreter",
        ),
        (ddog_trace_exporter_builder_set_hostname, "hostname"),
        (ddog_trace_exporter_builder_set_env, "test"),
        (ddog_trace_exporter_builder_set_app_version, "1.2"),
        (ddog_trace_exporter_builder_set_service, "service"),
        (ddog_trace_exporter_builder_set_process_tags, "key:val"),
        (ddog_trace_exporter_builder_set_runtime_id, "runtime-id"),
        (ddog_trace_exporter_builder_set_test_session_token, "token"),
        (
            ddog_trace_exporter_builder_set_otlp_endpoint,
            "http://localhost:4318/v1/traces",
        ),
        (
            ddog_trace_exporter_builder_set_otlp_protocol,
            "http/protobuf",
        ),
        (
            ddog_trace_exporter_builder_set_span_obfuscation_config,
            r#"{"http":{"remove_query_string":true}}"#,
        ),
        (
            ddog_trace_exporter_builder_set_dogstatsd_url,
            "http://localhost:8125",
        ),
        (ddog_trace_exporter_builder_set_git_commit_sha, "797e9ea"),
        (ddog_trace_exporter_builder_set_container_id, "cid"),
        (
            ddog_trace_exporter_builder_set_language_interpreter_vendor,
            "vendor",
        ),
        (
            ddog_trace_exporter_builder_set_otlp_metrics_endpoint,
            "http://localhost:4318/v1/metrics",
        ),
    ];
    for &(setter, value) in setters {
        unsafe {
            let error = setter(None, CharSlice::from(value));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);

            let mut config = TraceExporterBuilder::default();
            let before = format!("{config:?}");
            assert_eq!(setter(Some(&mut config), CharSlice::from(value)), None);
            let configured = format!("{config:?}");
            assert_ne!(
                configured, before,
                "setter must update the builder: {value}"
            );

            let error = setter(Some(&mut config), CharSlice::from_bytes(&[0xff]));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidInput);
            assert_eq!(
                format!("{config:?}"),
                configured,
                "invalid input must leave the builder unchanged"
            );
        }
    }
}

#[test]
fn config_constructor_test() {
    unsafe {
        let mut config = MaybeUninit::<Box<TraceExporterBuilder<ForkSafeRuntime>>>::uninit();
        ddog_trace_exporter_builder_new(NonNull::from(&mut config).cast());
        ddog_trace_exporter_builder_free(config.assume_init());
    }
}

#[test]
fn config_compute_stats_bucket_interval() {
    unsafe {
        let setter = ddog_trace_exporter_builder_enable_stats;
        assert_eq!(setter(None, 10).unwrap().code, ErrorCode::InvalidArgument);

        let mut config = TraceExporterBuilder::default();
        for bucket_interval_secs in [10, 30] {
            assert_eq!(setter(Some(&mut config), bucket_interval_secs), None);
            let mut expected = TraceExporterBuilder::default();
            expected.enable_stats(Duration::from_secs(bucket_interval_secs));
            assert_eq!(format!("{config:?}"), format!("{expected:?}"));
        }
    }
}

#[test]
fn config_client_computed_stats_enable_only() {
    unsafe {
        let setter = ddog_trace_exporter_builder_set_client_computed_stats;
        assert_eq!(setter(None).unwrap().code, ErrorCode::InvalidArgument);

        let mut config = TraceExporterBuilder::default();
        let mut expected = TraceExporterBuilder::default();
        expected.set_client_computed_stats();
        for _ in 0..2 {
            assert_eq!(setter(Some(&mut config)), None);
            assert_eq!(format!("{config:?}"), format!("{expected:?}"));
        }
    }
}

#[test]
fn config_health_metrics_enable_only() {
    unsafe {
        let setter = ddog_trace_exporter_builder_enable_health_metrics;
        assert_eq!(setter(None).unwrap().code, ErrorCode::InvalidArgument);

        let mut config = TraceExporterBuilder::default();
        let mut expected = TraceExporterBuilder::default();
        expected.enable_health_metrics();
        for _ in 0..2 {
            assert_eq!(setter(Some(&mut config)), None);
            assert_eq!(format!("{config:?}"), format!("{expected:?}"));
        }
    }
}

/// Each binding applies exactly the builder method it mirrors, so the configured builder
/// matches one configured through the Rust API.
#[test]
fn builder_flag_and_format_setters() {
    unsafe {
        // Formats and protocol toggles.
        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_input_format(
                Some(&mut config),
                TraceExporterInputFormat::V04
            ),
            None
        );
        let mut expected = TraceExporterBuilder::default();
        expected.set_input_format(TraceExporterInputFormat::V04);
        assert_eq!(format!("{config:?}"), format!("{expected:?}"));

        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_output_format(
                Some(&mut config),
                TraceExporterOutputFormat::V1
            ),
            None
        );
        let mut expected = TraceExporterBuilder::default();
        expected.set_output_format(TraceExporterOutputFormat::V1);
        assert_eq!(format!("{config:?}"), format!("{expected:?}"));

        // Unit and bool toggles.
        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_restart_after_fork(Some(&mut config), false),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_enable_v1_protocol(Some(&mut config)),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_client_computed_top_level(Some(&mut config)),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_enable_agent_rates_payload_version(Some(&mut config)),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_enable_otel_trace_semantics(Some(&mut config)),
            None
        );
        let mut expected = TraceExporterBuilder::default();
        expected
            .set_restart_after_fork(false)
            .enable_v1_protocol()
            .set_client_computed_top_level()
            .enable_agent_rates_payload_version()
            .enable_otel_trace_semantics();
        assert_eq!(format!("{config:?}"), format!("{expected:?}"));

        // Null builder is rejected.
        assert_eq!(
            ddog_trace_exporter_builder_set_input_format(None, TraceExporterInputFormat::V04)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_output_format(None, TraceExporterOutputFormat::V04)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_restart_after_fork(None, true)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        type UnitSetter = unsafe extern "C" fn(
            Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
        ) -> Option<Box<ExporterError>>;
        for f in [
            ddog_trace_exporter_builder_enable_v1_protocol as UnitSetter,
            ddog_trace_exporter_builder_set_client_computed_top_level as UnitSetter,
            ddog_trace_exporter_builder_enable_agent_rates_payload_version as UnitSetter,
            ddog_trace_exporter_builder_enable_otel_trace_semantics as UnitSetter,
        ] {
            assert_eq!(f(None).unwrap().code, ErrorCode::InvalidArgument);
        }
    }
}

#[test]
fn builder_tag_and_header_setters() {
    unsafe {
        let tags = [CharSlice::from("key1:val1"), CharSlice::from("key2:val2")];
        let tag_list = ExporterTagList {
            ptr: tags.as_ptr(),
            len: tags.len(),
        };
        let empty_tag_list = ExporterTagList {
            ptr: core::ptr::null(),
            len: 0,
        };

        type TagSetter = unsafe extern "C" fn(
            Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
            ExporterTagList<'_>,
        ) -> Option<Box<ExporterError>>;
        type RustTagSetter = fn(
            &mut TraceExporterBuilder<ForkSafeRuntime>,
            Vec<String>,
        ) -> &mut TraceExporterBuilder<ForkSafeRuntime>;
        let tag_setters: &[(TagSetter, RustTagSetter)] = &[
            (
                ddog_trace_exporter_builder_set_tracer_tags,
                |b, t: Vec<String>| b.set_tracer_tags(t),
            ),
            (
                ddog_trace_exporter_builder_set_peer_tags,
                |b, t: Vec<String>| b.set_peer_tags(t),
            ),
            (
                ddog_trace_exporter_builder_set_additional_metric_tag_keys,
                |b, t: Vec<String>| b.set_additional_metric_tag_keys(t),
            ),
        ];
        for &(ffi, rust) in tag_setters {
            assert_eq!(
                ffi(None, tag_list).unwrap().code,
                ErrorCode::InvalidArgument
            );
            // A null ptr with a non-zero len is rejected.
            assert_eq!(
                ffi(
                    None,
                    ExporterTagList {
                        ptr: core::ptr::null(),
                        len: 2
                    }
                )
                .unwrap()
                .code,
                ErrorCode::InvalidArgument
            );

            let mut config = TraceExporterBuilder::default();
            assert_eq!(ffi(Some(&mut config), tag_list), None);
            let mut expected = TraceExporterBuilder::default();
            rust(
                &mut expected,
                vec!["key1:val1".to_string(), "key2:val2".to_string()],
            );
            assert_eq!(format!("{config:?}"), format!("{expected:?}"));

            // An empty list clears the value.
            assert_eq!(ffi(Some(&mut config), empty_tag_list), None);
            let mut cleared = TraceExporterBuilder::default();
            rust(&mut cleared, Vec::new());
            let cleared = format!("{cleared:?}");
            assert_eq!(format!("{config:?}"), cleared);

            // Invalid UTF-8 in any element is rejected atomically.
            let invalid = [CharSlice::from("ok"), CharSlice::from_bytes(&[0xff])];
            assert_eq!(
                ffi(
                    Some(&mut config),
                    ExporterTagList {
                        ptr: invalid.as_ptr(),
                        len: invalid.len()
                    }
                )
                .unwrap()
                .code,
                ErrorCode::InvalidInput
            );
            assert_eq!(format!("{config:?}"), cleared);
        }

        let headers = [
            ExporterHeader {
                name: CharSlice::from("header-1"),
                value: CharSlice::from("value-1"),
            },
            ExporterHeader {
                name: CharSlice::from("header-2"),
                value: CharSlice::from("value-2"),
            },
        ];
        let header_list = ExporterHeaderList {
            ptr: headers.as_ptr(),
            len: headers.len(),
        };

        type HeaderSetter = unsafe extern "C" fn(
            Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
            ExporterHeaderList<'_>,
        ) -> Option<Box<ExporterError>>;
        type RustHeaderSetter = fn(
            &mut TraceExporterBuilder<ForkSafeRuntime>,
            Vec<(String, String)>,
        ) -> &mut TraceExporterBuilder<ForkSafeRuntime>;
        let header_setters: &[(HeaderSetter, RustHeaderSetter)] = &[
            (
                ddog_trace_exporter_builder_set_otlp_headers,
                |b, h: Vec<(String, String)>| b.set_otlp_headers(h),
            ),
            (
                ddog_trace_exporter_builder_set_otlp_metrics_headers,
                |b, h: Vec<(String, String)>| b.set_otlp_metrics_headers(h),
            ),
        ];
        let expected_pairs = vec![
            ("header-1".to_string(), "value-1".to_string()),
            ("header-2".to_string(), "value-2".to_string()),
        ];
        for &(ffi, rust) in header_setters {
            assert_eq!(
                ffi(None, header_list).unwrap().code,
                ErrorCode::InvalidArgument
            );
            assert_eq!(
                ffi(
                    None,
                    ExporterHeaderList {
                        ptr: core::ptr::null(),
                        len: 2
                    }
                )
                .unwrap()
                .code,
                ErrorCode::InvalidArgument
            );

            let mut config = TraceExporterBuilder::default();
            assert_eq!(ffi(Some(&mut config), header_list), None);
            let mut expected = TraceExporterBuilder::default();
            rust(&mut expected, expected_pairs.clone());
            assert_eq!(format!("{config:?}"), format!("{expected:?}"));

            // Invalid UTF-8 in either name or value is rejected atomically.
            let configured = format!("{config:?}");
            for invalid in [
                ExporterHeader {
                    name: CharSlice::from_bytes(&[0xff]),
                    value: CharSlice::from("value"),
                },
                ExporterHeader {
                    name: CharSlice::from("name"),
                    value: CharSlice::from_bytes(&[0xff]),
                },
            ] {
                assert_eq!(
                    ffi(
                        Some(&mut config),
                        ExporterHeaderList {
                            ptr: &invalid,
                            len: 1
                        }
                    )
                    .unwrap()
                    .code,
                    ErrorCode::InvalidInput
                );
                assert_eq!(format!("{config:?}"), configured);
            }
        }
    }
}

#[cfg(feature = "stats-obfuscation")]
#[test]
fn builder_stats_obfuscation_setters() {
    unsafe {
        // Agentless stats endpoint.
        let setter = ddog_trace_exporter_builder_set_agentless_stats_endpoint;
        assert_eq!(
            setter(None, CharSlice::from("url")).unwrap().code,
            ErrorCode::InvalidArgument
        );

        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            setter(
                Some(&mut config),
                CharSlice::from("https://trace.agent.example.com/api/v0.2/stats")
            ),
            None
        );
        let mut expected = TraceExporterBuilder::default();
        expected.set_agentless_stats_endpoint("https://trace.agent.example.com/api/v0.2/stats");
        let configured = format!("{config:?}");
        assert_eq!(configured, format!("{expected:?}"));

        // Invalid UTF-8 is rejected and leaves the builder unchanged.
        assert_eq!(
            setter(Some(&mut config), CharSlice::from_bytes(&[0xff]))
                .unwrap()
                .code,
            ErrorCode::InvalidInput
        );
        assert_eq!(format!("{config:?}"), configured);

        // Client-side stats obfuscation.
        let setter = ddog_trace_exporter_builder_enable_client_side_stats_obfuscation;
        assert_eq!(setter(None).unwrap().code, ErrorCode::InvalidArgument);

        let mut config = TraceExporterBuilder::default();
        assert_eq!(setter(Some(&mut config)), None);
        let mut expected = TraceExporterBuilder::default();
        expected.enable_client_side_stats_obfuscation();
        assert_eq!(format!("{config:?}"), format!("{expected:?}"));
    }
}

#[test]
fn config_two_string_setters_are_atomic() {
    type Setter = unsafe extern "C" fn(
        Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
        CharSlice<'_>,
        CharSlice<'_>,
    ) -> Option<Box<ExporterError>>;
    let setters: &[(Setter, &str, &str)] = &[
        (
            ddog_trace_exporter_builder_set_agentless_endpoint,
            "https://example.com/v1/input",
            "api-key",
        ),
        (
            ddog_trace_exporter_builder_set_otlp_instrumentation_scope,
            "dd-trace-js",
            "7.0.0",
        ),
    ];
    for &(setter, first, second) in setters {
        unsafe {
            assert_eq!(
                setter(None, CharSlice::from(first), CharSlice::from(second))
                    .unwrap()
                    .code,
                ErrorCode::InvalidArgument
            );
            let mut config = TraceExporterBuilder::default();
            let before = format!("{config:?}");
            assert_eq!(
                setter(
                    Some(&mut config),
                    CharSlice::from(first),
                    CharSlice::from(second)
                ),
                None
            );
            let configured = format!("{config:?}");
            assert_ne!(configured, before);
            for (first, second) in [
                (CharSlice::from(first), CharSlice::from_bytes(&[0xff])),
                (CharSlice::from_bytes(&[0xff]), CharSlice::from(second)),
            ] {
                assert_eq!(
                    setter(Some(&mut config), first, second).unwrap().code,
                    ErrorCode::InvalidInput
                );
                assert_eq!(format!("{config:?}"), configured);
            }
        }
    }
}

#[test]
fn config_protocol_and_json_validation() {
    unsafe {
        let mut config = TraceExporterBuilder::default();
        for protocol in ["http/json", "http/protobuf", "grpc"] {
            assert_eq!(
                ddog_trace_exporter_builder_set_otlp_protocol(
                    Some(&mut config),
                    CharSlice::from(protocol)
                ),
                None
            );
        }
        let before = format!("{config:?}");
        let error = ddog_trace_exporter_builder_set_otlp_protocol(
            Some(&mut config),
            CharSlice::from("grcp"),
        );
        assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);
        assert_eq!(format!("{config:?}"), before);
        let error = ddog_trace_exporter_builder_set_span_obfuscation_config(
            Some(&mut config),
            CharSlice::from("{not valid json}"),
        );
        assert_eq!(error.unwrap().code, ErrorCode::InvalidInput);
        assert_eq!(format!("{config:?}"), before);
    }
}

#[test]
fn builder_enable_telemetry_sets_only_telemetry_config() {
    unsafe {
        let mut config = TraceExporterBuilder::default();
        let before = format!("{config:?}");
        assert_eq!(
            ddog_trace_exporter_builder_enable_telemetry(Some(&mut config), None),
            None
        );
        assert_eq!(format!("{config:?}"), before);
        let telemetry = ExporterTelemetryConfig {
            heartbeat: 500,
            debug_enabled: true,
        };
        assert_eq!(
            ddog_trace_exporter_builder_enable_telemetry(None, Some(&telemetry))
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_enable_telemetry(Some(&mut config), Some(&telemetry)),
            None
        );
        // Only the telemetry config is applied; the sessions stay at their default.
        let mut expected = TraceExporterBuilder::default();
        expected.enable_telemetry(TelemetryConfig {
            heartbeat: 500,
            debug_enabled: true,
        });
        assert_eq!(format!("{config:?}"), format!("{expected:?}"));
        assert_ne!(format!("{config:?}"), before);
    }
}

#[test]
fn builder_set_telemetry_instrumentation_sessions() {
    unsafe {
        let setter = ddog_trace_exporter_builder_set_telemetry_instrumentation_sessions;
        let session = CharSlice::from("session");
        let root = CharSlice::from("root");
        let parent = CharSlice::from("parent");
        assert_eq!(
            setter(None, session, root, parent).unwrap().code,
            ErrorCode::InvalidArgument
        );

        let mut config = TraceExporterBuilder::default();
        assert_eq!(setter(Some(&mut config), session, root, parent), None);
        let mut expected = TraceExporterBuilder::default();
        expected.set_telemetry_instrumentation_sessions(TelemetryInstrumentationSessions {
            session_id: Some("session".to_string()),
            root_session_id: Some("root".to_string()),
            parent_session_id: Some("parent".to_string()),
        });
        let configured = format!("{config:?}");
        assert_eq!(configured, format!("{expected:?}"));

        // Invalid UTF-8 in any argument is rejected atomically.
        for invalid in [
            (CharSlice::from_bytes(&[0xff]), root, parent),
            (session, CharSlice::from_bytes(&[0xff]), parent),
            (session, root, CharSlice::from_bytes(&[0xff])),
        ] {
            assert_eq!(
                setter(Some(&mut config), invalid.0, invalid.1, invalid.2)
                    .unwrap()
                    .code,
                ErrorCode::InvalidInput
            );
            assert_eq!(format!("{config:?}"), configured);
        }
    }
}

#[test]
fn config_numeric_setters() {
    unsafe {
        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_connection_timeout(None, 1000)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_agentless_timeout(None, 1000)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(None, 0)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        let limits = ddog_trace_exporter_builder_default_stats_cardinality_limit();
        assert_eq!(
            ddog_trace_exporter_builder_set_stats_cardinality_limit(None, limits)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );

        assert_eq!(
            ddog_trace_exporter_builder_set_connection_timeout(Some(&mut config), 1000),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_agentless_timeout(Some(&mut config), 30000),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_stats_cardinality_limit(Some(&mut config), limits),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(Some(&mut config), 0),
            None
        );
        let default_cap = format!("{config:?}");
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(Some(&mut config), 4096),
            None
        );
        assert_ne!(format!("{config:?}"), default_cap);
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(Some(&mut config), 0),
            None
        );
        assert_eq!(format!("{config:?}"), default_cap);
    }
}

#[test]
fn config_mutable_metadata_test() {
    unsafe {
        let metadata = MutableMetadataHandle::default();
        let mut config = TraceExporterBuilder::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_mutable_metadata(None, Some(&metadata))
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_mutable_metadata(Some(&mut config), None)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_mutable_metadata(Some(&mut config), Some(&metadata)),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_runtime_id(Some(&mut config), CharSlice::from("rt-3")),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_process_tags(
                Some(&mut config),
                CharSlice::from("key:val")
            ),
            None
        );
        assert_eq!(metadata.load().runtime_id, "rt-3");
        assert_eq!(metadata.load().process_tags, "key:val");
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn config_shared_runtime_ownership() {
    unsafe {
        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());
        let raw = Arc::into_raw(runtime.clone());
        let handle = NonNull::new(raw.cast_mut());
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(None, handle)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(Some(&mut config), None)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(Some(&mut config), handle),
            None
        );
        assert_eq!(Arc::strong_count(&runtime), 3);
        drop(Arc::from_raw(raw));
        ddog_trace_exporter_builder_free(config);
        assert_eq!(Arc::strong_count(&runtime), 1);
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn exporter_constructor_test() {
    unsafe {
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(Some(&mut config), 0),
            None
        );
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        assert_eq!(
            ddog_trace_exporter_builder_build(Some(config), NonNull::from(&mut exporter).cast()),
            None
        );
        ddog_trace_exporter_free(exporter.assume_init());
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn exporter_constructor_error_consumes_config() {
    unsafe {
        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        config.set_shared_runtime(runtime.clone());
        assert_eq!(
            ddog_trace_exporter_builder_set_agentless_timeout(Some(&mut config), 1000),
            None
        );
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        let error =
            ddog_trace_exporter_builder_build(Some(config), NonNull::from(&mut exporter).cast());
        assert!(
            error.is_some(),
            "a timeout without an agentless endpoint must fail"
        );
        ddog_trace_exporter_error_free(error);
        assert_eq!(
            Arc::strong_count(&runtime),
            1,
            "failed builds must drop the builder"
        );
    }
}

#[test]
fn exporter_null_arguments() {
    unsafe {
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        assert_eq!(
            ddog_trace_exporter_builder_build(None, NonNull::from(&mut exporter).cast())
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_send(None, ByteSlice::from(b"dummy" as &[u8]), None)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
    }
}

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
#[test]
fn catch_panic_test() {
    let ret = catch_panic!(panic!("Panic!"), gen_error!(ErrorCode::Panic));
    assert_eq!(ret.unwrap().code, ErrorCode::Panic);
}

#[test]
#[cfg_attr(miri, ignore)]
fn exporter_send_check_rate_test() {
    unsafe {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.4/traces");
            then.status(200).body(
                r#"{
                "rate_by_service": {
                    "service:foo,env:staging": 1.0,
                    "service:,env:": 0.8
                }
            }"#,
            );
        });

        let mut cfg = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        cfg.set_url(&server.url("/"))
            .set_tracer_version("0.1")
            .set_language("lang")
            .set_language_version("0.1")
            .set_language_interpreter("interpreter")
            .set_hostname("hostname")
            .set_env("env-test")
            .set_app_version("1.0")
            .set_service("test-service");

        let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
        let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();
        let mut ret =
            ddog_trace_exporter_builder_build(Some(cfg), NonNull::new_unchecked(&mut ptr).cast());

        assert_eq!(ret, None);
        let exporter = ptr.assume_init();

        let data = rmp_serde::to_vec_named::<Vec<Vec<SpanSlice>>>(&vec![vec![]]).unwrap();
        let traces = ByteSlice::new(&data);
        ret = ddog_trace_exporter_send(
            Some(exporter.as_ref()),
            traces,
            Some(NonNull::new_unchecked(&mut response).cast()),
        );
        assert_eq!(ret, None);
        assert_eq!(
            String::from_utf8_lossy(&response.assume_init().body.unwrap()),
            r#"{
                "rate_by_service": {
                    "service:foo,env:staging": 1.0,
                    "service:,env:": 0.8
                }
            }"#,
        );
        ddog_trace_exporter_free(exporter);
    }
}

fn agent_config(url: &str) -> Box<TraceExporterBuilder<ForkSafeRuntime>> {
    let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
    let setters: &[(StringSetter, &str)] = &[
        (ddog_trace_exporter_builder_set_url, url),
        (ddog_trace_exporter_builder_set_tracer_version, "0.1"),
        (ddog_trace_exporter_builder_set_language, "lang"),
        (ddog_trace_exporter_builder_set_language_version, "0.1"),
        (
            ddog_trace_exporter_builder_set_language_interpreter,
            "interpreter",
        ),
        (ddog_trace_exporter_builder_set_hostname, "hostname"),
        (ddog_trace_exporter_builder_set_env, "env-test"),
        (ddog_trace_exporter_builder_set_app_version, "1.0"),
        (ddog_trace_exporter_builder_set_service, "test-service"),
    ];
    for &(setter, value) in setters {
        assert_eq!(
            unsafe { setter(Some(&mut config), CharSlice::from(value)) },
            None
        );
    }
    config
}

#[test]
#[cfg_attr(miri, ignore)]
fn exporter_send_empty_array_test() {
    // Test added due to ensure the exporter is able to send empty arrays because some tracers
    // (.NET) ping the agent with the aforementioned data type.
    unsafe {
        let server = MockServer::start();
        let response_body = r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#;

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .header("Datadog-Meta-Tracer-Version", "0.1")
                .header("Datadog-Meta-Lang", "lang")
                .header("Datadog-Meta-Lang-Version", "0.1")
                .header("Datadog-Meta-Lang-Interpreter", "interpreter")
                .path("/v0.4/traces");
            then.status(200).body(response_body);
        });

        let cfg = agent_config(&server.url("/"));

        let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
        let mut ret =
            ddog_trace_exporter_builder_build(Some(cfg), NonNull::new_unchecked(&mut ptr).cast());

        assert_eq!(ret, None);
        let exporter = ptr.assume_init();

        let data = vec![0x90];
        let traces = ByteSlice::new(&data);
        let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

        ret = ddog_trace_exporter_send(
            Some(exporter.as_ref()),
            traces,
            Some(NonNull::new_unchecked(&mut response).cast()),
        );
        mock_traces.assert();
        assert_eq!(ret, None);
        assert_eq!(
            String::from_utf8_lossy(&response.assume_init().body.unwrap()),
            response_body
        );

        ddog_trace_exporter_free(exporter);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn exporter_send_telemetry_test() {
    unsafe {
        let server = MockServer::start();
        let response_body = r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#;
        let mock_traces = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let mock_metrics = server.mock(|when, then| {
            when.method(POST)
                .path("/telemetry/proxy/api/v2/apmtelemetry")
                .header("dd-session-id", "session")
                .header("dd-root-session-id", "root")
                .header("dd-parent-session-id", "parent")
                .body_includes(r#""runtime_id":"foo""#)
                .body_includes(r#""metric":"trace_api."#);
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let mut cfg = agent_config(&server.url("/"));
        assert_eq!(
            ddog_trace_exporter_builder_enable_telemetry(
                Some(&mut cfg),
                Some(&ExporterTelemetryConfig {
                    heartbeat: 10000,
                    debug_enabled: true,
                }),
            ),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_telemetry_instrumentation_sessions(
                Some(&mut cfg),
                CharSlice::from("session"),
                CharSlice::from("root"),
                CharSlice::from("parent"),
            ),
            None
        );

        let error =
            ddog_trace_exporter_builder_set_runtime_id(Some(&mut cfg), CharSlice::from("foo"));
        assert!(error.is_none());

        let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
        let mut ret =
            ddog_trace_exporter_builder_build(Some(cfg), NonNull::new_unchecked(&mut ptr).cast());

        assert_eq!(ret, None);
        let exporter = ptr.assume_init();

        let data = vec![0x90];
        let traces = ByteSlice::new(&data);
        let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

        ret = ddog_trace_exporter_send(
            Some(exporter.as_ref()),
            traces,
            Some(NonNull::new_unchecked(&mut response).cast()),
        );
        mock_traces.assert();
        assert_eq!(ret, None);
        assert_eq!(
            String::from_utf8_lossy(&response.assume_init().body.unwrap()),
            response_body
        );

        ddog_trace_exporter_free(exporter);
        // It should receive 1 metrics payload (excluding heartbeats)
        mock_metrics.assert_calls(1);
    }
}
