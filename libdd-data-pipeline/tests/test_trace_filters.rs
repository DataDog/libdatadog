// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::{
    agent_info,
    trace_exporter::{TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat},
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
use rand::Rng;
use serde_json::json;

mod tracing_integration_tests {
    use super::*;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn trace_filters_snapshot_test() {
        const EXTRA_INFO: &str = r#"{
        "version":"1",
        "filter_tags": {"reject": ["my_ignore_tag"], "require": ["my_require_tag:true"]},
        "filter_tags_regex": {"reject": ["my_regex_ignore_tag:.*true.*"]},
        "ignore_resources": [".*IGNORED.*"]
    }"#;
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "trace_filters_snapshot_test";
        let test_agent = DatadogTestAgent::new(
            Some(relative_snapshot_path),
            None,
            &[("DD_AGENT_EXTRA_INFO", EXTRA_INFO)],
        )
        .await;
        let url = test_agent.get_base_uri().await;
        test_agent.start_session(snapshot_name, None).await;

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .enable_stats(Duration::from_secs(10))
            .set_env("staging")
            .set_language("nodejs")
            .set_language_interpreter("v8")
            .set_language_version("1.0")
            .set_service("test")
            .set_test_session_token(snapshot_name)
            .set_tracer_version("1.0")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_url(url.to_string().as_ref());

        let trace_exporter = builder
            .build_async::<NativeCapabilities>()
            .await
            .expect("Unable to build TraceExporter");
        let data = get_v04_trace_snapshot_test_payload();
        let timeout = Duration::from_secs(2);
        let start = Instant::now();
        loop {
            if std::time::Instant::now().duration_since(start) > timeout {
                panic!("Timeout waiting for agent info to be ready");
            }
            if agent_info::get_agent_info().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let response = trace_exporter.send_async(data.as_ref()).await;
        assert!(response.is_ok());

        tokio::task::spawn_blocking(move || drop(trace_exporter))
            .await
            .unwrap();

        let received_traces = test_agent.get_sent_traces().await;

        println!(
            "{}",
            serde_json::to_string_pretty(&received_traces).unwrap()
        );

        test_agent.assert_snapshot(snapshot_name).await;
    }
}

fn get_v04_trace_snapshot_test_payload() -> Vec<u8> {
    let traces = vec![
        trace_1_span(
            "passes_filters_first",
            "test",
            &[("my_require_tag", "true")],
        ),
        // This one gets filtered out because it matches an ignore_resources pattern
        trace_1_span(
            "ignored_resource",
            "test IGNORED resource test",
            &[("my_require_tag", "true")],
        ),
        // This one gets filtered out because one of its tag matches a reject filter_tag
        trace_1_span(
            "reject_filter_tag",
            "test ignored because of reject filter_tag",
            &[("my_ignore_tag", ""), ("my_require_tag", "true")],
        ),
        // This one gets filtered out because one of its tag matches a reject
        // regex_filter_tag
        trace_1_span(
            "reject_rejex_filter_tag",
            "test ignored because of reject regex_filter_tag",
            &[
                ("my_regex_ignore_tag", "something-true-something"),
                ("my_require_tag", "true"),
            ],
        ),
        // This one gets filtered out because it doesn't have my_require_tag:true
        trace_1_span(
            "missing_required_filter_tag",
            "test ignored because missing a required filter_tag",
            &[("a_useless_tag", "true")],
        ),
        // This one gets filtered out because it doesn't have my_require_tag:true
        trace_1_span(
            "missing_required_filter_tag_value",
            "test ignored because wrong value on filter_tag",
            &[("my_require_tag", "false")],
        ),
        trace_1_span(
            "passes_filters_last",
            "test2",
            &[("my_require_tag", "true")],
        ),
    ];
    rmp_serde::to_vec_named(&traces).unwrap()
}

pub fn trace_1_span(name: &str, resource: &str, meta: &[(&str, &str)]) -> Vec<serde_json::Value> {
    vec![span(name, resource, meta)]
}

pub fn span(name: &str, resource: &str, meta: &[(&str, &str)]) -> serde_json::Value {
    let trace_id: u32 = rand::thread_rng().gen();
    let span_id: u32 = rand::thread_rng().gen();
    let meta: HashMap<&str, &str> = HashMap::from_iter(meta.iter().copied());

    json!(
        {
            "name": name,
            "resource": resource,
            "meta": meta,
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_id": 0,
            "service": "test-service",
            "start": 0,
            "duration": 5,
            "error": 0,
            "metrics": {},
            "meta_struct": {},
            "span_links": [],
            "span_events": [],
        }
    )
}
