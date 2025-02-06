#[cfg(test)]
mod tracing_integration_tests {
    use data_pipeline::trace_exporter::TraceExporter;
    use datadog_trace_utils::send_data::SendData;
    use datadog_trace_utils::test_utils::create_test_json_span;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use datadog_trace_utils::tracer_payload::{
        DefaultTraceChunkProcessor, TraceEncoding, TracerPayloadParams,
    };
    use serde_json::json;
    use tokio::runtime::Runtime;
    use tokio::task;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let handle = tokio::runtime::Handle::current();

        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;
        // let test_agent_url = test_agent
        //     .get_uri_for_endpoint("v0.4/traces", Some("compare_v04_trace_snapshot_test"))
        //     .await;

        let test_agent_endpoint_uri = test_agent.get_uri_for_endpoint("v0.4/traces", None).await;
        // TODO: EK - This is a hack to get the base url and port for now
        let test_agent_endpoint_full_url = test_agent_endpoint_uri.to_string();
        let base_and_port = test_agent_endpoint_full_url
            .split("/v0.4/traces")
            .collect::<Vec<&str>>()[0];
        let url = base_and_port.to_string();
        println!("Test agent url: {}", url);

        let res = task::spawn_blocking(move || {
            let trace_exporter = TraceExporter::builder()
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .build()
                .expect("Unable to build TraceExporter");

            let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);

            span_1["metrics"] = json!({
                "_dd_metric1": 1.0,
                "_dd_metric2": 2.0
            });

            let span_2 = create_test_json_span(1234, 12343, 12341, 1, false);
            let mut root_span = create_test_json_span(1234, 12341, 0, 0, true);
            root_span["type"] = json!("web".to_owned());

            let encoded_data =
                rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap();
            let data = tinybytes::Bytes::from(encoded_data);
            let response = trace_exporter.send(data, 1);
        })
        .await;

        let _response = res.unwrap();
    }
}
