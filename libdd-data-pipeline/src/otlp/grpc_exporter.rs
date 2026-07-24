// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP gRPC trace exporter.
//!
//! Each send opens a fresh plaintext HTTP/2 connection driven by ephemeral per-request tasks and
//! dropped when the send completes, so no background task persists to be orphaned across `fork(2)`.
//! TLS (`https://`) is not supported.

use crate::otlp::config::OtlpGrpcTraceConfig;
use crate::trace_exporter::error::{BuilderErrorKind, RequestError, TraceExporterError};
use bytes::Bytes;
use http_body_util::{BodyExt, Collected};
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo};
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::TcpStream;
use tonic::body::Body as TonicBody;
use tonic::client::Grpc;
use tonic::metadata::{AsciiMetadataKey, AsciiMetadataValue};
use tonic::{Code, Request, Status};
use tower_service::Service;
use tracing::warn;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// tonic 0.14 moved `ProstCodec` to the separate `tonic-prost` crate; we hand-roll a minimal
/// codec here to avoid that extra dependency and keep tonic at `default-features = false`.
pub(crate) mod prost_codec {
    use prost::Message as ProstMessage;
    use std::marker::PhantomData;
    use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
    use tonic::Status;

    #[derive(Clone, Default)]
    pub(crate) struct ProstCodecImpl<Enc, Dec> {
        _phantom: PhantomData<(Enc, Dec)>,
    }

    pub(crate) struct ProstEncoder<T>(PhantomData<T>);
    impl<T> Default for ProstEncoder<T> {
        fn default() -> Self {
            Self(PhantomData)
        }
    }
    impl<T: ProstMessage> ProstEncoder<T> {
        // Shared with the `Encoder` impl below. tonic's `EncodeBuf`/`DecodeBuf` constructors are
        // private to the crate, so tests exercise this generic-over-`BufMut`/`Buf` core directly
        // instead of going through the `Encoder`/`Decoder` traits (see `codec_tests`).
        fn encode_into(item: T, dst: &mut impl bytes::BufMut) -> Result<(), Status> {
            item.encode(dst)
                .map_err(|e| Status::internal(format!("Failed to encode protobuf message: {e}")))
        }
    }
    impl<T: ProstMessage> Encoder for ProstEncoder<T> {
        type Item = T;
        type Error = Status;
        fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
            Self::encode_into(item, dst)
        }
    }

    pub(crate) struct ProstDecoder<T>(PhantomData<T>);
    impl<T> Default for ProstDecoder<T> {
        fn default() -> Self {
            Self(PhantomData)
        }
    }
    impl<T: ProstMessage + Default> ProstDecoder<T> {
        // See `ProstEncoder::encode_into`: generic over `Buf` so it is testable without tonic's
        // private `DecodeBuf` constructor.
        fn decode_from(src: &mut impl bytes::Buf) -> Result<Option<T>, Status> {
            // copy_to_bytes drains the whole buffer even if the backing store is non-contiguous.
            let buf = src.copy_to_bytes(src.remaining());
            T::decode(buf)
                .map(Some)
                .map_err(|e| Status::internal(format!("Failed to decode protobuf message: {e}")))
        }
    }
    impl<T: ProstMessage + Default> Decoder for ProstDecoder<T> {
        type Item = T;
        type Error = Status;
        fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<T>, Status> {
            Self::decode_from(src)
        }
    }

    impl<Enc, Dec> Codec for ProstCodecImpl<Enc, Dec>
    where
        Enc: ProstMessage + Default + Send + 'static,
        Dec: ProstMessage + Default + Send + 'static,
    {
        type Encode = Enc;
        type Decode = Dec;
        type Encoder = ProstEncoder<Enc>;
        type Decoder = ProstDecoder<Dec>;
        fn encoder(&mut self) -> Self::Encoder {
            ProstEncoder::default()
        }
        fn decoder(&mut self) -> Self::Decoder {
            ProstDecoder::default()
        }
    }

    #[cfg(test)]
    mod codec_tests {
        use super::{ProstCodecImpl, ProstDecoder, ProstEncoder};
        use bytes::BytesMut;
        use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::{
            ExportTraceServiceRequest, ExportTraceServiceResponse,
        };
        use libdd_trace_protobuf::opentelemetry::proto::trace::v1::ResourceSpans;

        // Round-trips through the `BufMut`/`Buf`-generic core (`encode_into`/`decode_from`) that
        // the `Encoder`/`Decoder` impls delegate to, over a plain `BytesMut`; see
        // `ProstEncoder::encode_into` for why the `Codec` traits can't be driven directly.
        #[test]
        fn prost_codec_round_trips() {
            let msg = ExportTraceServiceRequest {
                resource_spans: vec![ResourceSpans {
                    resource: None,
                    scope_spans: vec![],
                    schema_url: "https://example.com/schema".to_string(),
                }],
            };
            let mut buf = BytesMut::new();
            ProstEncoder::encode_into(msg.clone(), &mut buf).unwrap();
            assert!(!buf.is_empty());

            let out = ProstDecoder::decode_from(&mut buf).unwrap();
            assert_eq!(out, Some(msg));

            // Response type also compiles with the codec generics.
            let _ =
                ProstCodecImpl::<ExportTraceServiceRequest, ExportTraceServiceResponse>::default();
        }
    }
}

/// Custom gRPC transport: a `tower::Service` that dials a fresh h2c connection per request.
#[derive(Clone, Debug)]
pub(crate) struct H2Service {
    /// `host:port` dialed per request (plaintext h2c, prior knowledge).
    authority: Arc<str>,
}

impl Service<http::Request<TonicBody>> for H2Service {
    type Response = http::Response<Collected<Bytes>>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<TonicBody>) -> Self::Future {
        let authority = self.authority.clone();
        Box::pin(async move {
            let tcp = TcpStream::connect(authority.as_ref()).await?;
            tcp.set_nodelay(true)?;
            let io = TokioIo::new(tcp);
            // handshake() spawns the socket-driver task (ephemeral) via TokioExecutor.
            let (mut sender, conn) = http2::Builder::new(TokioExecutor::new())
                .adaptive_window(true)
                .handshake::<_, TonicBody>(io)
                .await?;
            // The Connection (request dispatcher) must be driven or send_request deadlocks.
            // Spawn it ephemerally; it ends when `sender` drops at the close of this future.
            let driver = tokio::spawn(async move {
                let _ = conn.await;
            });
            let resp = sender.send_request(req).await?;
            let (parts, incoming) = resp.into_parts();
            let collected = incoming.collect().await?;
            driver.abort();
            Ok(http::Response::from_parts(parts, collected))
        })
    }
}

/// A gRPC transport for OTLP trace export: per-request config, request origin, and the dial
/// service. Holds no live connection (nothing to rebuild across fork).
#[derive(Clone, Debug)]
pub(crate) struct OtlpGrpcTransport {
    pub(crate) config: OtlpGrpcTraceConfig,
    origin: http::Uri,
    service: H2Service,
    /// Custom headers parsed to gRPC metadata once at build time; invalid entries are dropped.
    metadata_headers: Vec<(AsciiMetadataKey, AsciiMetadataValue)>,
}

/// Validate a gRPC endpoint (plaintext `http://` only) and build the transport.
// Not yet wired to the trace exporter's send loop; exercised by tests only.
#[allow(dead_code)]
pub(crate) fn build_grpc_transport(
    endpoint_url: &str,
    config: OtlpGrpcTraceConfig,
) -> Result<OtlpGrpcTransport, TraceExporterError> {
    let uri = endpoint_url.parse::<http::Uri>()?;

    let scheme = uri.scheme().ok_or_else(|| {
        TraceExporterError::Builder(BuilderErrorKind::InvalidUri(
            "gRPC endpoint must include a URI scheme".to_string(),
        ))
    })?;
    if scheme == &http::uri::Scheme::HTTPS {
        return Err(TraceExporterError::Builder(
            BuilderErrorKind::InvalidConfiguration(
                "gRPC TLS (https://) is not supported; use http:// and terminate TLS in a proxy \
                 in front of this endpoint if encryption is required"
                    .to_string(),
            ),
        ));
    }
    if scheme != &http::uri::Scheme::HTTP {
        return Err(TraceExporterError::Builder(
            BuilderErrorKind::InvalidConfiguration(format!(
                "unsupported gRPC endpoint scheme {scheme}; expected http"
            )),
        ));
    }
    let authority = uri.authority().ok_or_else(|| {
        TraceExporterError::Builder(BuilderErrorKind::InvalidUri(
            "gRPC endpoint must include an authority".to_string(),
        ))
    })?;
    let service = H2Service {
        authority: Arc::from(authority.as_str()),
    };

    // Origin = scheme+authority + normalized path prefix; tonic appends the RPC method path.
    let prefix = uri.path().trim_end_matches('/').to_string();
    let mut origin_parts = uri.clone().into_parts();
    origin_parts.path_and_query = if prefix.is_empty() {
        Some(http::uri::PathAndQuery::from_static("/"))
    } else {
        Some(prefix.parse()?)
    };
    let origin = http::Uri::from_parts(origin_parts)
        .map_err(|e| TraceExporterError::Builder(BuilderErrorKind::InvalidUri(e.to_string())))?;

    // Parse custom headers to gRPC metadata once here rather than on every send. Invalid entries
    // are skipped with a single warning (logging only the key: a value may carry a secret).
    let metadata_headers = config
        .headers
        .iter()
        .filter_map(|(k, v)| {
            match (
                k.parse::<AsciiMetadataKey>(),
                v.parse::<AsciiMetadataValue>(),
            ) {
                (Ok(key), Ok(val)) => Some((key, val)),
                _ => {
                    warn!("Skipping invalid gRPC metadata header: {k:?}");
                    None
                }
            }
        })
        .collect();

    Ok(OtlpGrpcTransport {
        config,
        origin,
        service,
        metadata_headers,
    })
}

const GRPC_EXPORT_PATH: &str = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";
const CLIENT_COMPUTED_STATS_HEADER: &str = "datadog-client-computed-stats";

type ExportCodec =
    prost_codec::ProstCodecImpl<ExportTraceServiceRequest, ExportTraceServiceResponse>;

/// Send one OTLP trace export request over gRPC. Bounds connect + RPC with a single timeout.
// Not yet wired to the trace exporter's send loop; exercised by tests only.
#[allow(dead_code)]
pub(crate) async fn send_otlp_traces_grpc(
    transport: &OtlpGrpcTransport,
    test_token: Option<&str>,
    client_computed_stats: bool,
    request: ExportTraceServiceRequest,
) -> Result<(), TraceExporterError> {
    let mut req = Request::new(request);
    attach_metadata(
        &mut req,
        &transport.metadata_headers,
        test_token,
        client_computed_stats,
    );

    let path = http::uri::PathAndQuery::from_static(GRPC_EXPORT_PATH);
    let codec = ExportCodec::default();

    tokio::time::timeout(transport.config.timeout, async {
        let mut client = Grpc::with_origin(transport.service.clone(), transport.origin.clone());
        client.ready().await.map_err(|e| {
            TraceExporterError::Io(std::io::Error::other(format!("gRPC not ready: {e}")))
        })?;
        client
            .unary(req, path, codec)
            .await
            .map(|_resp| ())
            .map_err(grpc_status_to_error)
    })
    .await
    .map_err(|_| TraceExporterError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)))?
}

// Insert the pre-validated custom headers, the optional test-session token, and (when enabled) the
// client-computed-stats marker into the request metadata.
fn attach_metadata(
    req: &mut Request<ExportTraceServiceRequest>,
    headers: &[(AsciiMetadataKey, AsciiMetadataValue)],
    test_token: Option<&str>,
    client_computed_stats: bool,
) {
    for (key, val) in headers {
        req.metadata_mut().insert(key.clone(), val.clone());
    }
    if let Some(token) = test_token {
        match token.parse::<AsciiMetadataValue>() {
            Ok(val) => {
                req.metadata_mut().insert(
                    AsciiMetadataKey::from_static("x-datadog-test-session-token"),
                    val,
                );
            }
            Err(_) => warn!("Skipping invalid test-session token"),
        }
    }
    if client_computed_stats {
        req.metadata_mut().insert(
            AsciiMetadataKey::from_static(CLIENT_COMPUTED_STATS_HEADER),
            AsciiMetadataValue::from_static("yes"),
        );
    }
}

fn grpc_status_to_error(status: Status) -> TraceExporterError {
    // Transport/IO failures from our bare `H2Service` fall through to `Code::Unknown` with the
    // original `std::io::Error` somewhere in the source chain (directly for a connect failure, or
    // wrapped in a `hyper::Error` for a post-connect handshake/read/write failure). Walk the chain
    // and recover it so these map to `Io` rather than an application-level `Request` error.
    if status.code() == Code::Unknown {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = status.source();
        while let Some(err) = cause {
            if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
                return TraceExporterError::Io(std::io::Error::new(
                    io_err.kind(),
                    io_err.to_string(),
                ));
            }
            cause = err.source();
        }
    }
    match status.code() {
        Code::Unavailable => TraceExporterError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            status.message(),
        )),
        Code::DeadlineExceeded => TraceExporterError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            status.message(),
        )),
        _ => TraceExporterError::Request(RequestError::new(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            status.message(),
        )),
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use std::time::Duration;

    fn cfg() -> OtlpGrpcTraceConfig {
        OtlpGrpcTraceConfig {
            headers: vec![],
            timeout: Duration::from_secs(5),
            otel_trace_semantics_enabled: false,
        }
    }

    #[test]
    fn rejects_https() {
        let err = build_grpc_transport("https://localhost:4317", cfg())
            .unwrap_err()
            .to_string();
        assert!(err.contains("TLS") || err.contains("https"), "got: {err}");
    }

    #[test]
    fn rejects_malformed_url() {
        assert!(build_grpc_transport("not a url", cfg()).is_err());
    }

    #[test]
    fn accepts_http_without_a_runtime() {
        assert!(build_grpc_transport("http://localhost:4317", cfg()).is_ok());
    }

    #[test]
    fn normalizes_origin_path_prefix() {
        let t = build_grpc_transport("http://localhost:4317/otel/", cfg()).unwrap();
        assert_eq!(t.origin.path(), "/otel");
    }

    #[test]
    fn build_skips_invalid_headers_and_keeps_valid() {
        let config = OtlpGrpcTraceConfig {
            headers: vec![
                ("good-key".to_string(), "ok".to_string()),
                // Invalid metadata key (contains a space): skipped, not fatal to the build.
                ("bad key".to_string(), "v".to_string()),
            ],
            timeout: Duration::from_secs(5),
            otel_trace_semantics_enabled: false,
        };
        // A malformed header must not fail the build; it is dropped and the valid one retained.
        let t = build_grpc_transport("http://localhost:4317", config).unwrap();
        assert_eq!(t.metadata_headers.len(), 1);
        assert_eq!(t.metadata_headers[0].0.as_str(), "good-key");
        assert_eq!(t.metadata_headers[0].1.to_str().unwrap(), "ok");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use bytes::Bytes;
    use h2::server;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message as _;
    use std::time::Duration;
    use tokio::net::TcpListener;

    fn cfg() -> OtlpGrpcTraceConfig {
        OtlpGrpcTraceConfig {
            headers: vec![],
            timeout: Duration::from_secs(2),
            otel_trace_semantics_enabled: false,
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn connection_refused_maps_to_io() {
        // Port 1 has no listener → connect fails.
        let transport = build_grpc_transport("http://127.0.0.1:1", cfg()).unwrap();
        let err = send_otlp_traces_grpc(
            &transport,
            None,
            false,
            ExportTraceServiceRequest::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TraceExporterError::Io(_)), "got: {err:?}");
    }

    // Minimal in-process gRPC server: accepts one unary Export call, decodes the request, and
    // replies OK with an empty ExportTraceServiceResponse plus a `grpc-status` trailer.
    async fn run_one_shot_grpc_server(listener: TcpListener) -> ExportTraceServiceRequest {
        let (socket, _) = listener.accept().await.unwrap();
        let mut conn = server::handshake(socket).await.unwrap();
        let (req, mut respond) = conn.accept().await.unwrap().unwrap();
        // Drain the gRPC-framed request body: 1-byte compression flag + 4-byte length + message.
        let mut body = req.into_body();
        let mut buf = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk.unwrap();
            buf.extend_from_slice(&chunk);
            body.flow_control().release_capacity(chunk.len()).unwrap();
        }
        let decoded = ExportTraceServiceRequest::decode(&buf[5..]).unwrap();

        // Respond: headers, one empty framed message, then grpc-status trailer.
        let resp = http::Response::builder()
            .status(200)
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let mut send = respond.send_response(resp, false).unwrap();
        let msg = ExportTraceServiceResponse::default();
        let mut framed = vec![0u8; 5];
        msg.encode(&mut framed).unwrap();
        let len = (framed.len() - 5) as u32;
        framed[1..5].copy_from_slice(&len.to_be_bytes());
        send.send_data(Bytes::from(framed), false).unwrap();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        send.send_trailers(trailers).unwrap();
        // Keep the connection future alive (driving any remaining H2 protocol traffic, e.g. the
        // client's final stream-closing frames) until the client drops its side and this
        // `accept()` resolves to `None`, so the connection task doesn't outlive the test.
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.accept()).await;
        decoded
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn sends_and_server_decodes_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(run_one_shot_grpc_server(listener));

        let transport = build_grpc_transport(&format!("http://{addr}"), cfg()).unwrap();
        let mut request = ExportTraceServiceRequest::default();
        request.resource_spans.push(Default::default());

        send_otlp_traces_grpc(&transport, None, false, request.clone())
            .await
            .expect("send should succeed");

        let decoded = server.await.unwrap();
        assert_eq!(decoded.resource_spans.len(), 1);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn timeout_maps_to_io_timedout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept the connection but never drive the H2 handshake or send a response, so the send
        // blocks until the timeout fires rather than completing or failing fast.
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let config = OtlpGrpcTraceConfig {
            headers: vec![],
            timeout: Duration::from_millis(150),
            otel_trace_semantics_enabled: false,
        };
        let transport = build_grpc_transport(&format!("http://{addr}"), config).unwrap();
        let err = send_otlp_traces_grpc(
            &transport,
            None,
            false,
            ExportTraceServiceRequest::default(),
        )
        .await
        .unwrap_err();
        server.abort();
        match err {
            TraceExporterError::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "got: {e:?}")
            }
            other => panic!("expected Io(TimedOut), got {other:?}"),
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn post_connect_transport_failure_maps_to_io() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Complete the H2 handshake and accept the request, then abort the TCP connection with a
        // RST (SO_LINGER=0) instead of a graceful close or a gRPC-status trailer. The client's
        // in-flight read then fails with ECONNRESET. This is the FIX-1 case: the resulting
        // `std::io::Error` is wrapped inside a `hyper::Error` below the tonic `Status`, so it is
        // recovered only by walking the source chain in `grpc_status_to_error` — a graceful FIN
        // instead surfaces as an h2 "canceled" status that would be misreported as `Request`.
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Force a RST on close rather than a graceful FIN. `set_linger` is deprecated because a
            // non-zero linger blocks the thread on drop; a zero duration instead drops buffered
            // data and sends the RST immediately without blocking, which is exactly what we want.
            #[allow(deprecated)]
            socket.set_linger(Some(Duration::ZERO)).unwrap();
            let mut conn = server::handshake(socket).await.unwrap();
            let (req, _respond) = conn.accept().await.unwrap().unwrap();
            // Drain the request body so the client is parked awaiting the response when we reset.
            let mut body = req.into_body();
            while let Some(chunk) = body.data().await {
                let chunk = chunk.unwrap();
                body.flow_control().release_capacity(chunk.len()).unwrap();
            }
            // Drop the connection (and its socket) -> RST.
            drop(conn);
        });

        let transport = build_grpc_transport(&format!("http://{addr}"), cfg()).unwrap();
        let err = send_otlp_traces_grpc(
            &transport,
            None,
            false,
            ExportTraceServiceRequest::default(),
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(
            matches!(err, TraceExporterError::Io(_)),
            "expected Io (post-connect transport failure), got: {err:?}"
        );
    }
}

#[cfg(test)]
mod send_tests {
    use super::*;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
    use tonic::{Code, Request, Status};

    #[test]
    fn status_transient_maps_to_io_kind() {
        for (s, want) in [
            (
                Status::unavailable("down"),
                std::io::ErrorKind::ConnectionRefused,
            ),
            (
                Status::deadline_exceeded("slow"),
                std::io::ErrorKind::TimedOut,
            ),
        ] {
            match grpc_status_to_error(s) {
                TraceExporterError::Io(e) => assert_eq!(e.kind(), want),
                other => panic!("expected Io, got {other:?}"),
            }
        }
    }

    #[test]
    fn status_unknown_with_io_source_maps_to_io() {
        // Exercises the `Code::Unknown` + io-source recovery path in `grpc_status_to_error`.
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let status = Status::from_error(Box::new(io_err));
        assert_eq!(status.code(), Code::Unknown);
        match grpc_status_to_error(status) {
            TraceExporterError::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused)
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn status_unknown_without_io_source_maps_to_request() {
        assert!(matches!(
            grpc_status_to_error(Status::new(Code::Unknown, "mystery")),
            TraceExporterError::Request(_)
        ));
    }

    #[test]
    fn status_application_errors_map_to_request() {
        assert!(matches!(
            grpc_status_to_error(Status::new(Code::Internal, "boom")),
            TraceExporterError::Request(_)
        ));
    }

    #[test]
    fn attach_metadata_inserts_headers_token_and_stats() {
        let mut req = Request::new(ExportTraceServiceRequest::default());
        let headers = vec![(
            AsciiMetadataKey::from_static("good-key"),
            AsciiMetadataValue::from_static("ok"),
        )];
        attach_metadata(&mut req, &headers, Some("tok"), true);
        assert_eq!(req.metadata().get("good-key").unwrap(), "ok");
        assert_eq!(
            req.metadata().get("x-datadog-test-session-token").unwrap(),
            "tok"
        );
        assert_eq!(
            req.metadata().get("datadog-client-computed-stats").unwrap(),
            "yes"
        );
    }
}
