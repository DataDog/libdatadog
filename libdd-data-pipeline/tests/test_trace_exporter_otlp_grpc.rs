// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(all(test, not(target_arch = "wasm32")))]
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
    use tokio::sync::oneshot;
    use tokio::task::JoinSet;

    struct ReceivedExport {
        path: String,
        client_computed_stats: Option<String>,
        request: ExportTraceServiceRequest,
    }

    async fn run_grpc_test_server(
        listener: TcpListener,
        req_tx: mpsc::Sender<ReceivedExport>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { return };
                    let connection_req_tx = req_tx.clone();
                    connections.spawn(async move {
                        let Ok(mut connection) = server::handshake(socket).await else {
                            return;
                        };
                        let mut handlers = JoinSet::new();
                        while let Some(result) = connection.accept().await {
                            if let Ok((request, respond)) = result {
                                handlers.spawn(handle_export_stream(
                                    request,
                                    respond,
                                    connection_req_tx.clone(),
                                ));
                            }
                        }
                        while let Some(result) = handlers.join_next().await {
                            result.expect("gRPC request handler failed");
                        }
                    });
                }
            }
        }
        connections.abort_all();
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                assert!(error.is_cancelled(), "gRPC connection task failed: {error}");
            }
        }
    }

    async fn handle_export_stream(
        request: http::Request<h2::RecvStream>,
        mut respond: h2::server::SendResponse<Bytes>,
        req_tx: mpsc::Sender<ReceivedExport>,
    ) {
        let path = request.uri().path().to_string();
        let client_computed_stats = request
            .headers()
            .get("datadog-client-computed-stats")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = request.into_body();
        let mut frame_data: Vec<u8> = Vec::new();
        while let Some(chunk) = body.data().await {
            let Ok(chunk) = chunk else { return };
            let len = chunk.len();
            frame_data.extend_from_slice(&chunk);
            body.flow_control().release_capacity(len).ok();
        }

        let decoded = if frame_data.len() > 5 {
            ExportTraceServiceRequest::decode(&frame_data[5..]).ok()
        } else {
            None
        };
        if let Some(req) = decoded {
            let _ = req_tx.send(ReceivedExport {
                path,
                client_computed_stats,
                request: req,
            });
        }

        let response_proto = ExportTraceServiceResponse::default();
        let proto_bytes = response_proto.encode_to_vec();
        let mut frame = Vec::with_capacity(5 + proto_bytes.len());
        frame.push(0u8);
        frame.extend_from_slice(
            &u32::try_from(proto_bytes.len())
                .expect("protobuf response exceeds the gRPC frame length")
                .to_be_bytes(),
        );
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

        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        let _ = send_stream.send_trailers(trailers);
    }

    fn resource_attribute<'a>(
        request: &'a ExportTraceServiceRequest,
        key: &str,
    ) -> Option<&'a str> {
        request
            .resource_spans
            .first()
            .and_then(|rs| rs.resource.as_ref())
            .and_then(|r| {
                r.attributes.iter().find_map(|kv| {
                    if kv.key == key {
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

    fn run_grpc_export_end_to_end_and_survives_shared_runtime_restart<'scope, 'env: 'scope>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
    ) {
        let (port_tx, port_rx) = mpsc::channel::<u16>();
        let (req_tx, req_rx) = mpsc::channel::<ReceivedExport>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let server = scope.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx.send(listener.local_addr().unwrap().port()).unwrap();
                run_grpc_test_server(listener, req_tx, shutdown_rx).await;
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
            .set_service("grpc-test-svc")
            .set_client_computed_stats();

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
            resource_attribute(&initial.request, "service.name"),
            Some("grpc-test-svc"),
            "service.name attribute not found or wrong value"
        );
        assert_eq!(initial.client_computed_stats.as_deref(), Some("yes"));
        assert_eq!(
            resource_attribute(&initial.request, "_dd.stats_computed"),
            Some("true")
        );

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
            resource_attribute(&after_restart.request, "service.name"),
            Some("grpc-test-svc"),
            "service.name attribute not found or wrong value after runtime restart"
        );
        shutdown_tx.send(()).expect("server stopped early");
        server.join().expect("server thread failed");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn grpc_export_end_to_end_and_survives_shared_runtime_restart() {
        std::thread::scope(|scope| {
            run_grpc_export_end_to_end_and_survives_shared_runtime_restart(scope)
        });
    }
}
