// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(unix, not(miri)))]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::{OtlpProtocol, trace_exporter::TraceExporterBuilder};
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
use libdd_trace_utils::test_utils::create_test_json_span;
use prost::Message;
use std::{convert::Infallible, time::Duration};
use tokio::net::UnixListener;

#[tokio::test]
async fn otlp_http_json_and_metrics_export_over_unix_socket() {
    check_http_export(OtlpProtocol::HttpJson, "application/json").await;
}

#[tokio::test]
async fn otlp_http_protobuf_and_metrics_export_over_unix_socket() {
    check_http_export(OtlpProtocol::HttpProtobuf, "application/x-protobuf").await;
}

async fn check_http_export(protocol: OtlpProtocol, content_type: &'static str) {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let path = directory.path().join("otel.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        for (protocol, content_type, resource_field) in [
            (protocol, content_type, "resourceSpans"),
            (
                OtlpProtocol::HttpJson,
                "application/json",
                "resourceMetrics",
            ),
        ] {
            let (socket, _) = listener.accept().await.unwrap();
            http1::Builder::new()
                .keep_alive(false)
                .serve_connection(
                    TokioIo::new(socket),
                    service_fn(move |request: Request<Incoming>| async move {
                        assert_eq!(request.method(), "POST");
                        assert_eq!(request.uri().path(), "/");
                        assert_eq!(request.headers()["content-type"], content_type);
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        match protocol {
                            OtlpProtocol::HttpJson => {
                                let payload: serde_json::Value =
                                    serde_json::from_slice(&body).unwrap();
                                assert!(!payload[resource_field].as_array().unwrap().is_empty());
                            }
                            OtlpProtocol::HttpProtobuf => {
                                let payload = ExportTraceServiceRequest::decode(body).unwrap();
                                assert!(!payload.resource_spans.is_empty());
                            }
                            OtlpProtocol::Grpc => unreachable!(),
                        }
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                    }),
                )
                .await
                .unwrap();
        }
    });

    let endpoint = format!("unix://{}", path.display());
    tokio::task::spawn_blocking(move || {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_otlp_endpoint(&endpoint)
            .set_otlp_protocol(protocol)
            .set_otlp_metrics_endpoint(&endpoint)
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let span = create_test_json_span(1234, 12342, 0, 1, false);
        let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        exporter.send(data.as_ref()).unwrap();
        exporter.shutdown(None).unwrap();
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
