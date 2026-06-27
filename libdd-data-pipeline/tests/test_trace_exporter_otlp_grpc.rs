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
    use std::sync::mpsc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// Runs the in-process h2 server loop: accept one connection, decode the first
    /// Export request, forward it on `req_tx`, and return a gRPC success response.
    async fn run_grpc_test_server(
        listener: TcpListener,
        req_tx: mpsc::Sender<ExportTraceServiceRequest>,
    ) {
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
                let _ = req_tx.send(req);
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

        // Briefly drive the connection so the client receives the response frames
        // (including the grpc-status trailer) before the socket closes. Bounded so
        // the server task can never loop forever waiting on a pooled client
        // connection that never closes.
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while h2_conn.accept().await.is_some() {}
        })
        .await;
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn grpc_export_sends_decodable_request() {
        // The h2 server runs on its own OS thread + runtime, and the exporter
        // drives its own runtime via `send` on this thread. Keeping them on
        // separate threads (rather than one shared test runtime) means neither
        // can starve the other under parallel CI load — the previous
        // `#[tokio::test]` current-thread setup could deadlock the server task.
        let (port_tx, port_rx) = mpsc::channel::<u16>();
        let (req_tx, req_rx) = mpsc::channel::<ExportTraceServiceRequest>();

        // The server thread is detached and time-bounded end to end (60s ceiling):
        // if the client never connects, the bound fires and the thread exits rather
        // than lingering or blocking. The test never joins it — verification comes
        // from `send` returning Ok (the client received grpc-status:0) plus the
        // decoded request delivered on `req_rx`.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let _ = tokio::time::timeout(Duration::from_secs(60), async move {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    port_tx.send(listener.local_addr().unwrap().port()).unwrap();
                    run_grpc_test_server(listener, req_tx).await;
                })
                .await;
            });
        });

        let port = port_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server did not bind within 10s");
        let endpoint = format!("http://127.0.0.1:{port}");

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_otlp_endpoint(&endpoint)
            .set_otlp_protocol(OtlpProtocol::Grpc)
            .set_connection_timeout(Some(30_000))
            .set_language("test-lang")
            .set_tracer_version("1.0")
            .set_env("grpc-test-env")
            .set_service("grpc-test-svc");
        let exporter = builder.build::<NativeCapabilities>().expect("build");

        let mut span = create_test_json_span(1234, 12342, 12341, 1, false);
        span["service"] = json!("grpc-test-svc");
        span["name"] = json!("grpc_span");
        let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // No ambient async runtime on this thread, so the exporter's internal
        // `block_on` runs directly without `spawn_blocking`.
        exporter.send(data.as_ref()).expect("send ok");

        // The server decodes and forwards the request before responding, so it is
        // already available once `send` returns.
        let received = req_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server did not receive a decodable request within 10s");

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
}
