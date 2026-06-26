// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test for OTLP gRPC trace export.
//!
//! Starts an in-process HTTP/2 gRPC server using the `h2` crate, configures
//! a [`TraceExporter`] with `OtlpProtocol::Grpc`, sends a trace, and asserts
//! that the server received a valid [`ExportTraceServiceRequest`] containing
//! the expected span.

#[cfg(test)]
mod grpc_export_tests {
    use bytes::Bytes;
    use h2::server;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::{trace_exporter::TraceExporterBuilder, OtlpProtocol};
    use libdd_trace_protobuf::opentelemetry::proto::{
        collector::trace::v1::{ExportTraceServiceRequest, ExportTraceServiceResponse},
        common::v1::any_value::Value,
    };
    use libdd_trace_utils::test_utils::create_test_json_span;
    use prost::Message;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task;

    /// Starts a minimal HTTP/2 gRPC server that accepts exactly one Export call.
    ///
    /// Returns `(port, rx)` where `rx` receives the decoded
    /// `ExportTraceServiceRequest` after the server handles the first request.
    async fn start_grpc_test_server() -> (u16, oneshot::Receiver<ExportTraceServiceRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = oneshot::channel::<ExportTraceServiceRequest>();

        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut h2_conn = server::handshake(socket).await.unwrap();

            if let Some(result) = h2_conn.accept().await {
                let (request, mut respond) = result.unwrap();

                // Collect the full request body. The chunk length must be captured
                // before `extend_from_slice` consumes it to satisfy the borrow checker.
                let mut body = request.into_body();
                let mut frame_data: Vec<u8> = Vec::new();
                while let Some(chunk) = body.data().await {
                    let chunk = chunk.unwrap();
                    let len = chunk.len();
                    frame_data.extend_from_slice(&chunk);
                    body.flow_control().release_capacity(len).ok();
                }

                // Decode the gRPC frame: skip the 5-byte prefix (1 compression flag + 4 length).
                let decoded = if frame_data.len() > 5 {
                    ExportTraceServiceRequest::decode(&frame_data[5..]).ok()
                } else {
                    None
                };
                if let Some(req) = decoded {
                    let _ = tx.send(req);
                }

                // Send gRPC success response: 200 headers + protobuf body + grpc-status trailer.
                let response_proto = ExportTraceServiceResponse::default();
                let proto_bytes = response_proto.encode_to_vec();
                let mut frame = Vec::with_capacity(5 + proto_bytes.len());
                frame.push(0u8); // no compression
                frame.extend_from_slice(&(proto_bytes.len() as u32).to_be_bytes());
                frame.extend_from_slice(&proto_bytes);

                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap();
                let mut send_stream = respond.send_response(response, false).unwrap();
                send_stream.send_data(Bytes::from(frame), false).unwrap();

                // gRPC-status trailer (trailing HEADERS frame with END_STREAM).
                let mut trailers = http::HeaderMap::new();
                trailers.insert("grpc-status", "0".parse().unwrap());
                send_stream.send_trailers(trailers).unwrap();
            }

            // Drive the connection to completion so the client receives all frames
            // before the TCP socket closes.
            while h2_conn.accept().await.is_some() {}
        });

        (port, rx)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn grpc_export_sends_decodable_request() {
        let (port, rx) = start_grpc_test_server().await;

        let endpoint = format!("http://127.0.0.1:{port}");

        // TraceExporter::send internally drives a tokio runtime; use spawn_blocking
        // so it does not block the test's async runtime.
        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&endpoint)
                .set_otlp_protocol(OtlpProtocol::Grpc)
                .set_language("test-lang")
                .set_tracer_version("1.0")
                .set_env("grpc-test-env")
                .set_service("grpc-test-svc");
            let exporter = builder.build::<NativeCapabilities>().expect("build");

            let mut span = create_test_json_span(1234, 12342, 12341, 1, false);
            span["service"] = json!("grpc-test-svc");
            span["name"] = json!("grpc_span");
            let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
            exporter.send(data.as_ref()).expect("send ok");
        })
        .await;

        assert!(
            task_result.is_ok(),
            "exporter task panicked: {task_result:?}"
        );

        // Wait for the server to receive and decode the request (5 second timeout).
        let received = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .expect("server did not receive request within 5s")
            .expect("server channel closed without sending");

        // Validate the decoded request contains the expected span.
        assert!(
            !received.resource_spans.is_empty(),
            "expected at least one ResourceSpans"
        );
        let service_name = received
            .resource_spans
            .first()
            .and_then(|rs| rs.resource.as_ref())
            .and_then(|r| {
                r.attributes.iter().find_map(|kv| {
                    if kv.key == "service.name" {
                        kv.value
                            .as_ref()
                            .and_then(|v| v.value.as_ref())
                            .and_then(|v| match v {
                                Value::StringValue(s) => Some(s.as_str()),
                                _ => None,
                            })
                    } else {
                        None
                    }
                })
            });
        assert_eq!(
            service_name,
            Some("grpc-test-svc"),
            "service.name attribute not found or wrong value"
        );
    }

    /// Verify the protocol string "grpc" is accepted by `OtlpProtocol`'s `FromStr` impl.
    #[test]
    fn grpc_protocol_string_parses() {
        use std::str::FromStr;
        let protocol = OtlpProtocol::from_str("grpc");
        assert!(
            matches!(protocol, Ok(OtlpProtocol::Grpc)),
            "expected OtlpProtocol::Grpc, got {protocol:?}"
        );
    }
}
