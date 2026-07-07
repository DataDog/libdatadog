// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod otlp_protobuf_tests {
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::trace_exporter::TraceExporterBuilder;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
    use libdd_trace_utils::test_utils::create_test_json_span;
    use prost::Message;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::task;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_protobuf_export_sends_decodable_payload() {
        use httpmock::MockServer;

        // The httpmock 0.8 alpha API does not expose captured request bodies after the fact, so
        // we decode and validate the protobuf body inside a custom request matcher. The matcher
        // flips `body_valid` when the payload decodes and carries the expected service.name.
        let body_valid = Arc::new(AtomicBool::new(false));
        let matcher_flag = body_valid.clone();

        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(move |when, then| {
                let flag = matcher_flag.clone();
                when.method("POST")
                    .path("/v1/traces")
                    .header("content-type", "application/x-protobuf")
                    .is_true(move |req: &httpmock::prelude::HttpMockRequest| {
                        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value;
                        let Ok(decoded) = ExportTraceServiceRequest::decode(req.body_ref()) else {
                            return false;
                        };
                        let valid = decoded
                            .resource_spans
                            .first()
                            .and_then(|rs| rs.resource.as_ref())
                            .map(|resource| {
                                resource.attributes.iter().any(|kv| {
                                    kv.key == "service.name"
                                        && matches!(
                                            kv.value.as_ref().and_then(|v| v.value.as_ref()),
                                            Some(Value::StringValue(s)) if s == "test"
                                        )
                                })
                            })
                            .unwrap_or(false);
                        if valid {
                            flag.store(true, Ordering::SeqCst);
                        }
                        valid
                    });
                then.status(200).body("");
            })
            .await;

        let endpoint = format!("http://localhost:{}/v1/traces", server.port());
        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&endpoint)
                .set_otlp_protocol(libdd_data_pipeline::OtlpProtocol::HttpProtobuf)
                .set_language("test-lang")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test");
            let exporter = builder.build::<NativeCapabilities>().expect("build");
            let mut span = create_test_json_span(1234, 12342, 12341, 1, false);
            span["name"] = json!("pb_span");
            let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
            exporter.send(data.as_ref()).expect("send ok");
        })
        .await;

        assert!(task_result.is_ok());
        assert_eq!(mock.calls_async().await, 1);
        assert!(
            body_valid.load(Ordering::SeqCst),
            "protobuf body did not decode to the expected ExportTraceServiceRequest"
        );
    }
}
