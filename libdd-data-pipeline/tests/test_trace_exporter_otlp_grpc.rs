// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod grpc_export_tests {
    use bytes::Bytes;
    use h2::server;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::{trace_exporter::TraceExporterBuilder, OtlpProtocol};
    use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
    use libdd_trace_protobuf::opentelemetry::proto::{
        collector::trace::v1::{ExportTraceServiceRequest, ExportTraceServiceResponse},
        common::v1::any_value::Value,
    };
    use libdd_trace_utils::test_utils::create_test_json_span;
    use prost::Message;
    use serde_json::json;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    use tokio::net::TcpListener;

    struct ReceivedExport {
        path: String,
        request: ExportTraceServiceRequest,
    }

    /// The `accept()` loop must keep polling for the connection's lifetime: that is what drives
    /// incoming DATA-frame reads and flushes queued response frames, so an in-flight request would
    /// hang if it stopped.
    async fn run_grpc_test_server(listener: TcpListener, req_tx: mpsc::Sender<ReceivedExport>) {
        while let Ok((socket, _)) = listener.accept().await {
            let connection_req_tx = req_tx.clone();
            tokio::spawn(async move {
                let Ok(mut h2_conn) = server::handshake(socket).await else {
                    return;
                };
                while let Some(result) = h2_conn.accept().await {
                    if let Ok((request, respond)) = result {
                        tokio::spawn(handle_export_stream(
                            request,
                            respond,
                            connection_req_tx.clone(),
                        ));
                    }
                }
            });
        }
    }

    /// Decode one gRPC Export request and reply with a success response + trailer.
    async fn handle_export_stream(
        request: http::Request<h2::RecvStream>,
        mut respond: h2::server::SendResponse<Bytes>,
        req_tx: mpsc::Sender<ReceivedExport>,
    ) {
        let path = request.uri().path().to_string();
        // Collect the full request body. The chunk length must be captured before
        // `extend_from_slice` consumes it to satisfy the borrow checker.
        let mut body = request.into_body();
        let mut frame_data: Vec<u8> = Vec::new();
        while let Some(chunk) = body.data().await {
            let Ok(chunk) = chunk else { return };
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
            let _ = req_tx.send(ReceivedExport { path, request: req });
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
        let Ok(mut send_stream) = respond.send_response(response, false) else {
            return;
        };
        let _ = send_stream.send_data(Bytes::from(frame), false);

        // gRPC-status trailer (trailing HEADERS frame with END_STREAM).
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        let _ = send_stream.send_trailers(trailers);
    }

    /// Extract the `service.name` resource attribute's string value from a decoded request, if
    /// present on the first `ResourceSpans` entry.
    fn service_name(request: &ExportTraceServiceRequest) -> Option<&str> {
        request
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
            })
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn grpc_export_end_to_end_and_survives_shared_runtime_restart() {
        // The h2 server runs on its own OS thread + current-thread runtime; the exporter's
        // `send` drives its own shared runtime on this thread. Keeping them on separate
        // threads means neither can starve the other under parallel CI load.
        let (port_tx, port_rx) = mpsc::channel::<u16>();
        let (req_tx, req_rx) = mpsc::channel::<ReceivedExport>();

        // The server thread is detached and time-bounded end to end (60s ceiling): if the
        // client never connects, the bound fires and the thread exits rather than lingering or
        // blocking. The test never joins it — verification comes from `send` returning Ok (the
        // client observed grpc-status: 0) plus the decoded request delivered on `req_rx`.
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
        let endpoint = format!("http://127.0.0.1:{port}/otel/");
        let expected_path = "/otel/opentelemetry.proto.collector.trace.v1.TraceService/Export";

        let shared_runtime = Arc::new(ForkSafeRuntime::new().expect("build shared runtime"));

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_shared_runtime(shared_runtime.clone())
            .set_otlp_endpoint(&endpoint)
            .set_otlp_protocol(OtlpProtocol::Grpc)
            .set_connection_timeout(Some(30_000))
            .set_language("test-lang")
            .set_tracer_version("1.0")
            .set_env("grpc-test-env")
            .set_service("grpc-test-svc");

        // `build()` is the sync facade over `build_async`: it drives setup on `shared_runtime`
        // itself (via `block_on`), so no ambient tokio context is required here. The gRPC
        // transport dials lazily per-send, so `build()` does not touch the network.
        let exporter = builder
            .build::<NativeCapabilities>()
            .expect("build exporter");

        let mut span = create_test_json_span(1234, 12342, 12341, 1, false);
        span["service"] = json!("grpc-test-svc");
        span["name"] = json!("grpc_span");
        let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();

        exporter.send(data.as_ref()).expect("initial send ok");
        let initial = req_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server did not receive the initial request");
        assert_eq!(initial.path, expected_path);
        assert!(
            !initial.request.resource_spans.is_empty(),
            "expected at least one ResourceSpans"
        );
        assert_eq!(
            service_name(&initial.request),
            Some("grpc-test-svc"),
            "service.name attribute not found or wrong value"
        );

        // Regression guard: the base transport dials a fresh h2c connection per send and holds
        // no persistent channel or background worker, so restarting the exporter's shared
        // runtime mid-lifecycle must not break subsequent sends.
        shared_runtime.before_fork();
        shared_runtime
            .after_fork_parent()
            .expect("restart shared runtime");

        exporter.send(data.as_ref()).expect("post-restart send ok");
        let after_restart = req_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server did not receive the post-restart request");
        assert_eq!(after_restart.path, expected_path);
        assert_eq!(
            service_name(&after_restart.request),
            Some("grpc-test-svc"),
            "service.name attribute not found or wrong value after runtime restart"
        );
    }
}
