// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use datadog_trace_utils::send_data::SendData;
    use datadog_trace_utils::test_utils::create_test_span;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
    use ddcommon::Endpoint;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let relative_snapshot_path = "trace-utils/tests/snapshots/";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path)).await;

        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            client_computed_top_level: false,
            client_computed_stats: false,
        };

        let endpoint = Endpoint {
            url: test_agent
                .get_uri_for_endpoint("v0.4/traces", Some("compare_v04_trace_snapshot_test"))
                .await,
            api_key: None,
        };

        let trace = vec![create_test_span(1234, 12342, 12341, 1, false)];

        let data = SendData::new(
            100,
            TracerPayloadCollection::V04(vec![trace.clone()]),
            header_tags,
            &endpoint,
        );

        let _result = data.send().await;

        test_agent
            .assert_snapshot("compare_v04_trace_snapshot_test")
            .await;
    }
}
