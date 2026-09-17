// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{Duration, Instant};

use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::{
    agent_info,
    trace_exporter::{TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat},
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
use serde_json::json;

// Each preserved token exercises a non-mode option from the agent's nested SQL config.
const SQL_RESOURCE: &str = "SELECT * FROM users WHERE email = 'alice@example.com' AND active = TRUE AND deleted_at IS NULL;";

fn sql_trace_payload() -> Vec<u8> {
    let span = json!({
        "service": "test-service",
        "name": "postgres.query",
        "resource": SQL_RESOURCE,
        "type": "sql",
        "trace_id": 1,
        "span_id": 1,
        "parent_id": 0,
        "start": 1_000_000_000,
        "duration": 5_000_000,
        "error": 0,
        "meta": {},
        "metrics": {"_sampling_priority_v1": 1.0},
        "meta_struct": {},
        "span_links": [],
        "span_events": [],
    });

    rmp_serde::to_vec_named(&vec![vec![span]]).expect("trace payload must serialize")
}

async fn wait_for_agent_info() {
    let timeout = Duration::from_secs(5);
    let start = Instant::now();
    while agent_info::get_agent_info().is_none() {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for the test agent /info response"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn client_side_stats_follows_agent_sql_config_snapshot_test() {
    // Keep both modes identical: TRUE, NULL, and the trailing semicolon can only survive if the
    // nested keep_* options are followed, rather than only the legacy sql_obfuscation_mode.
    const EXTRA_INFO: &str = r#"{
        "obfuscation_version": 2,
        "config": {
            "obfuscation": {
                "sql_obfuscation_mode": "obfuscate_and_normalize",
                "sql": {
                    "obfuscation_mode": "obfuscate_and_normalize",
                    "keep_boolean": true,
                    "keep_null": true,
                    "keep_trailing_semicolon": true
                }
            }
        }
    }"#;
    const SNAPSHOT_DIRECTORY: &str = "libdd-data-pipeline/tests/snapshots/";
    const SNAPSHOT_NAME: &str = "client_side_stats_follows_agent_sql_config_snapshot_test";

    let test_agent = DatadogTestAgent::new(
        Some(SNAPSHOT_DIRECTORY),
        None,
        &[("DD_AGENT_EXTRA_INFO", EXTRA_INFO)],
    )
    .await;
    let url = test_agent.get_base_uri().await;
    test_agent.start_session(SNAPSHOT_NAME, None).await;

    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(url.to_string().as_ref())
        .set_env("staging")
        .set_service("test-service")
        .set_language("rust")
        .set_language_version("1.0")
        .set_language_interpreter("rustc")
        .set_tracer_version("1.0")
        .set_test_session_token(SNAPSHOT_NAME)
        .set_input_format(TraceExporterInputFormat::V04)
        .set_output_format(TraceExporterOutputFormat::V04)
        .enable_stats(Duration::from_secs(10))
        .enable_client_side_stats_obfuscation();

    let exporter = builder
        .build_async::<NativeCapabilities>()
        .await
        .expect("trace exporter must build");

    wait_for_agent_info().await;
    exporter
        .send_async(&sql_trace_payload())
        .await
        .expect("trace must be sent");
    tokio::task::spawn_blocking(move || exporter.shutdown(None))
        .await
        .expect("trace exporter shutdown task must complete")
        .expect("trace exporter must flush stats and shut down");

    test_agent.assert_snapshot(SNAPSHOT_NAME).await;
}
