// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP gRPC trace exporter.
//!
//! Sends an [`ExportTraceServiceRequest`] over a tonic gRPC channel using
//! plaintext HTTP/2 (`http://` scheme). TLS (`https://`) is not yet supported.
//!
//! # gRPC framing
//! The inner [`ProstCodecImpl`] + tonic's [`Grpc`](tonic::client::Grpc) handle
//! the 5-byte frame prefix, protobuf encoding, and gRPC trailer parsing
//! automatically, using [`prost`] for message encoding/decoding.

use super::config::OtlpGrpcTraceConfig;
use crate::trace_exporter::error::{BuilderErrorKind, RequestError, TraceExporterError};
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use prost::Message as ProstMessage;
use std::marker::PhantomData;
use std::time::Duration;
use tonic::{
    client::Grpc,
    codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    metadata::{AsciiMetadataKey, AsciiMetadataValue},
    transport::{Channel, Endpoint},
    Code, Request, Status,
};
use tracing::warn;

/// gRPC path for the OTLP trace export RPC.
const GRPC_EXPORT_PATH: &str = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";

/// Metadata header signalling that the client already computed APM stats, so the
/// agent does not recompute them. Attached per-request when stats are enabled.
const CLIENT_COMPUTED_STATS_HEADER: &str = "datadog-client-computed-stats";

// ---------------------------------------------------------------------------
// Prost codec implementation — tonic 0.14 removed ProstCodec from its public
// API; we provide a minimal replacement that satisfies tonic::codec::Codec.
// ---------------------------------------------------------------------------

/// A [`tonic::codec::Codec`] that encodes and decodes prost messages.
#[derive(Clone, Default)]
pub(crate) struct ProstCodecImpl<Enc, Dec> {
    _phantom: PhantomData<(Enc, Dec)>,
}

/// A prost message encoder that implements [`tonic::codec::Encoder`].
pub(crate) struct ProstEncoder<T> {
    _phantom: PhantomData<T>,
}

impl<T: Default> Default for ProstEncoder<T> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T: ProstMessage> Encoder for ProstEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("Failed to encode protobuf message: {e}")))
    }
}

/// A prost message decoder that implements [`tonic::codec::Decoder`].
pub(crate) struct ProstDecoder<T> {
    _phantom: PhantomData<T>,
}

impl<T: Default> Default for ProstDecoder<T> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T: ProstMessage + Default> Decoder for ProstDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        use bytes::Buf as _;
        // `copy_to_bytes` drains the whole buffer correctly even if `DecodeBuf`'s
        // backing store is ever non-contiguous (`chunk()` only returns the first
        // contiguous segment, so the old chunk()+advance() form could truncate).
        let buf = src.copy_to_bytes(src.remaining());
        match T::decode(buf) {
            Ok(msg) => Ok(Some(msg)),
            Err(e) => Err(Status::internal(format!(
                "Failed to decode protobuf message: {e}"
            ))),
        }
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

// ---------------------------------------------------------------------------
// OtlpGrpcTransport
// ---------------------------------------------------------------------------

/// A connected gRPC transport for OTLP trace export.
///
/// Holds the per-export config and a lazily-connected tonic [`Channel`].
/// Clone is cheap — `Channel` is internally reference-counted.
#[derive(Clone, Debug)]
pub(crate) struct OtlpGrpcTransport {
    pub(crate) config: OtlpGrpcTraceConfig,
    /// Lazily-connected HTTP/2 channel. tonic establishes the TCP connection
    /// on the first RPC call and maintains a connection pool afterwards.
    pub(crate) channel: Channel,
}

/// Build a lazy tonic gRPC channel for `endpoint_url`.
///
/// The channel does **not** connect eagerly — TCP setup happens on the first
/// RPC call. `timeout` is stored on the channel and applied per-request.
///
/// Only `http://` scheme endpoints are accepted; `https://` is not yet
/// supported (use a TLS-terminating sidecar for encrypted connections).
pub(crate) fn build_grpc_channel(
    endpoint_url: &str,
    timeout: Duration,
) -> Result<Channel, TraceExporterError> {
    if endpoint_url.starts_with("https://") {
        return Err(TraceExporterError::Builder(
            BuilderErrorKind::InvalidConfiguration(
                "gRPC TLS (https://) is not yet supported; use http:// and a \
                 TLS-terminating sidecar if encryption is required"
                    .to_string(),
            ),
        ));
    }
    let channel = Endpoint::from_shared(endpoint_url.to_owned())
        .map_err(|e| TraceExporterError::Builder(BuilderErrorKind::InvalidUri(e.to_string())))?
        .timeout(timeout)
        .connect_lazy(); // Non-async; connects on first RPC call.
    Ok(channel)
}

/// Send an OTLP trace export request over gRPC.
///
/// Uses the `transport.channel` tonic channel with [`ProstCodecImpl`] for
/// encoding/decoding. Custom metadata headers, the test session token, and
/// (when `client_computed_stats` is set) the client-computed-stats header are
/// attached to the request metadata.
///
/// # Errors
///
/// Returns [`TraceExporterError::Io`] on timeout or connection failure,
/// [`TraceExporterError::Request`] on non-OK gRPC status codes.
pub(crate) async fn send_otlp_traces_grpc(
    transport: &OtlpGrpcTransport,
    test_token: Option<&str>,
    client_computed_stats: bool,
    request: ExportTraceServiceRequest,
) -> Result<(), TraceExporterError> {
    let mut client = Grpc::new(transport.channel.clone());

    let mut req = Request::new(request);
    attach_metadata(
        &mut req,
        &transport.config.headers,
        test_token,
        client_computed_stats,
    );

    let path = http::uri::PathAndQuery::from_static(GRPC_EXPORT_PATH);
    let codec = ProstCodecImpl::<ExportTraceServiceRequest, ExportTraceServiceResponse>::default();

    // Tower's `Buffer` service (used inside tonic's `Channel`) requires
    // `poll_ready` to be called and return `Ready` before `call`. tonic's
    // `Grpc::ready()` drives that poll loop for us.
    tokio::time::timeout(transport.config.timeout, async {
        client.ready().await.map_err(|e| {
            TraceExporterError::Io(std::io::Error::other(format!(
                "gRPC channel not ready: {e}"
            )))
        })?;
        client
            .unary(req, path, codec)
            .await
            .map(|_response| ())
            .map_err(grpc_status_to_error)
    })
    .await
    .map_err(|_| TraceExporterError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)))?
}

/// Attach `headers`, the optional test-session token, and the optional
/// client-computed-stats marker to gRPC request metadata.
///
/// Invalid custom headers are skipped with a warning rather than failing the
/// export — a single malformed user-supplied header should not drop the batch.
fn attach_metadata(
    req: &mut Request<ExportTraceServiceRequest>,
    headers: &[(String, String)],
    test_token: Option<&str>,
    client_computed_stats: bool,
) {
    for (k, v) in headers {
        match (
            k.parse::<AsciiMetadataKey>(),
            v.parse::<AsciiMetadataValue>(),
        ) {
            (Ok(key), Ok(val)) => {
                req.metadata_mut().insert(key, val);
            }
            _ => warn!("Skipping invalid gRPC metadata header: {k:?}={v:?}"),
        }
    }
    if let Some(token) = test_token {
        match token.parse::<AsciiMetadataValue>() {
            Ok(val) => {
                req.metadata_mut().insert(
                    AsciiMetadataKey::from_static("x-datadog-test-session-token"),
                    val,
                );
            }
            Err(_) => warn!("Skipping invalid test-session token: {token:?}"),
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
    match status.code() {
        Code::Ok => {
            // Ok status should never reach the error path — tonic's `unary`
            // returns Ok(response) on Code::Ok, so map_err is not called.
            TraceExporterError::Io(std::io::Error::other(
                "gRPC Ok status reached error handler (unexpected)",
            ))
        }
        // Server temporarily unreachable / overloaded.
        Code::Unavailable => TraceExporterError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            status.message(),
        )),
        // Server-side deadline fired — a timeout, not a refused connection.
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
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn build_grpc_channel_rejects_https() {
        let err = build_grpc_channel("https://localhost:4317", Duration::from_secs(10));
        assert!(
            err.is_err(),
            "https:// should be rejected until TLS is implemented"
        );
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("TLS") || msg.contains("https"), "got: {msg}");
    }

    /// `connect_lazy()` internally registers with the Tokio reactor — wrap in a runtime.
    #[tokio::test]
    async fn build_grpc_channel_accepts_http() {
        // connect_lazy() doesn't dial — this should always succeed.
        let result = build_grpc_channel("http://localhost:4317", Duration::from_secs(10));
        assert!(result.is_ok(), "http:// must produce a channel: {result:?}");
    }

    #[test]
    fn build_grpc_channel_rejects_malformed_url() {
        let err = build_grpc_channel("not a url", Duration::from_secs(10));
        assert!(err.is_err());
    }

    #[test]
    fn grpc_status_transient_map_to_io_with_correct_kind() {
        // Unavailable → ConnectionRefused; DeadlineExceeded → TimedOut (not refused).
        for (status, want) in [
            (
                Status::unavailable("backend down"),
                std::io::ErrorKind::ConnectionRefused,
            ),
            (
                Status::deadline_exceeded("too slow"),
                std::io::ErrorKind::TimedOut,
            ),
        ] {
            match grpc_status_to_error(status) {
                TraceExporterError::Io(e) => assert_eq!(e.kind(), want),
                other => panic!("expected Io, got {other:?}"),
            }
        }
    }

    #[test]
    fn grpc_status_application_errors_map_to_request() {
        for status in [
            Status::internal("boom"),
            Status::invalid_argument("bad"),
            Status::resource_exhausted("quota"),
        ] {
            assert!(
                matches!(grpc_status_to_error(status), TraceExporterError::Request(_)),
                "non-transient status should map to Request"
            );
        }
    }

    #[test]
    fn attach_metadata_inserts_valid_header_and_token() {
        let mut req = Request::new(ExportTraceServiceRequest::default());
        let headers = vec![("x-custom-key".to_string(), "val".to_string())];
        attach_metadata(&mut req, &headers, Some("tok123"), false);
        let md = req.metadata();
        assert_eq!(md.get("x-custom-key").unwrap(), "val");
        assert_eq!(md.get("x-datadog-test-session-token").unwrap(), "tok123");
        assert!(md.get(CLIENT_COMPUTED_STATS_HEADER).is_none());
    }

    #[test]
    fn attach_metadata_skips_invalid_header_keeps_valid() {
        let mut req = Request::new(ExportTraceServiceRequest::default());
        // A space in the key is not a valid HTTP/2 header name, so it is skipped.
        let headers = vec![
            ("bad key".to_string(), "v".to_string()),
            ("good-key".to_string(), "ok".to_string()),
        ];
        attach_metadata(&mut req, &headers, None, false);
        assert_eq!(req.metadata().get("good-key").unwrap(), "ok");
        assert_eq!(req.metadata().len(), 1, "only the valid header is attached");
    }

    #[test]
    fn attach_metadata_adds_client_computed_stats_when_enabled() {
        let mut req = Request::new(ExportTraceServiceRequest::default());
        attach_metadata(&mut req, &[], None, true);
        assert_eq!(
            req.metadata().get(CLIENT_COMPUTED_STATS_HEADER).unwrap(),
            "yes"
        );
    }

    /// `connect_lazy()` requires a Tokio runtime — wrap in `#[tokio::test]`.
    #[tokio::test]
    async fn grpc_transport_is_clone() {
        let channel = build_grpc_channel("http://localhost:4317", Duration::from_secs(5))
            .expect("http channel must build");
        let transport = OtlpGrpcTransport {
            config: OtlpGrpcTraceConfig {
                headers: vec![],
                timeout: Duration::from_secs(5),
                otel_trace_semantics_enabled: false,
            },
            channel,
        };
        let _clone = transport.clone();
    }
}
