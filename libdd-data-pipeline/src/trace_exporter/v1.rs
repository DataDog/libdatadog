// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! TraceExporter methods for the v1 span model.

use super::agent_response::AgentResponse;
use super::error::{InternalErrorKind, TraceExporterError};
use super::log_writer::write_log_traces_v1;
use super::{
    OtlpExportMode, PayloadCounts, TraceExporter, TraceExporterOutputFormat, V1_TRACES_ENDPOINT,
    stats,
};
#[cfg(not(target_arch = "wasm32"))]
use super::{grpc_retry_delay, grpc_retry_jitter};
use crate::agentless::AgentlessTraceConfig;
use crate::agentless::exporter::send_agentless_traces_with_observer_v1;
use crate::otlp::OtlpTraceConfig;
use crate::otlp::exporter::{OTLP_MAX_RETRIES, send_otlp_http_with_observer};
#[cfg(not(target_arch = "wasm32"))]
use crate::otlp::{GrpcExportError, OtlpGrpcTransport, send_otlp_traces_grpc};
use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_common::Endpoint;
#[cfg(not(target_arch = "wasm32"))]
use libdd_shared_runtime::BlockingRuntime;
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::msgpack_encoder;
use libdd_trace_utils::otlp_encoder::mapper_v1::map_traces_to_otlp_v1;
use libdd_trace_utils::span::trace_utils::SDK_OTLP_EXPORT_KEY;
use libdd_trace_utils::span::v1::span_pool::PooledTraceChunks;
use libdd_trace_utils::span::v1::{AttributeValue, TraceChunk};
use libdd_trace_utils::span::v04::VecMap;
use libdd_trace_utils::span::{SpanText, TraceData, trace_utils_v1};
use libdd_trace_utils::trace_utils::TracerHeaderTags;
#[cfg(all(feature = "telemetry", not(target_arch = "wasm32")))]
use prost::Message;
use std::borrow::Borrow;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

impl PayloadCounts {
    fn from_v1_chunks<T: TraceData>(traces: &[TraceChunk<T>]) -> Self {
        Self {
            chunks: traces.len(),
            #[cfg(feature = "telemetry")]
            spans: traces.iter().map(|c| c.spans.len()).sum(),
        }
    }
}

impl<
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> TraceExporter<C, R>
{
    /// Send a list of v1 trace chunks to the agent (or OTLP endpoint when configured).
    ///
    /// Sync facade over [`Self::send_trace_chunks_async_v1`]; panics inside an existing tokio
    /// context.
    ///
    /// `trace_chunks` accepts [`PooledTraceChunks`] so a caller can back it with a real
    /// [`libdd_trace_utils::span::v1::span_pool::SpanPoolV1`] to recycle span allocations; no
    /// caller in this codebase does so yet (today's production callers all go through msgpack),
    /// wiring one up is left to a future change.
    ///
    /// # Arguments
    /// * trace_chunks: A list of native v1 trace chunks.
    /// * cancellation_token: When provided, cancelling the token aborts the send while it is in
    ///   progress. The send only observes a token that is cancelled while the request is in-flight;
    ///   a token cancelled before this call returns immediately, and a token cancelled after the
    ///   send has already finished has no effect. Cancelling an in-flight send may cause the trace
    ///   chunks being sent to be lost.
    ///
    /// # Returns
    /// * Ok(AgentResponse): The response from the agent (or Unchanged for OTLP)
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_trace_chunks_v1<T: TraceData>(
        &self,
        trace_chunks: PooledTraceChunks<'_, T>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<AgentResponse, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        self.shared_runtime
            .block_on(self.send_trace_chunks_cancellable_v1(trace_chunks, cancellation_token))?
    }

    /// Send chunks, giving up with an `Interrupted` error once the token is cancelled.
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_trace_chunks_cancellable_v1<T: TraceData>(
        &self,
        trace_chunks: PooledTraceChunks<'_, T>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<AgentResponse, TraceExporterError> {
        match cancellation_token {
            Some(token) => {
                tokio::select! {
                    res = self.send_trace_chunks_async_v1(trace_chunks) => res,
                    _ = token.cancelled() => Err(TraceExporterError::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "send cancelled via cancellation token",
                    ))),
                }
            }
            None => self.send_trace_chunks_async_v1(trace_chunks).await,
        }
    }

    /// V1-native counterpart of [`Self::send_trace_chunks_async`]. There is no byte-decode path
    /// for v1: tracer identity always comes from this exporter's own metadata (the v0.4 path has
    /// no such identity to carry either: v0.4 chunks are plain span lists with no tracer-level
    /// metadata of their own).
    pub async fn send_trace_chunks_async_v1<T: TraceData>(
        &self,
        trace_chunks: PooledTraceChunks<'_, T>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // There is no agent to negotiate with, skip the poll.
        if self.log_output.is_none() && self.agentless_config.is_none() {
            self.check_agent_info().await;
        }
        self.process_and_send_trace_chunks_v1(trace_chunks).await
    }

    /// Sends v1 trace chunks to the Datadog agentless intake (`/v1/input`) as JSON.
    ///
    /// V1-native counterpart of [`Self::send_agentless_traces_inner`]. See
    /// [`send_agentless_traces_with_observer_v1`] for the (temporary) obfuscation gap on this
    /// path.
    async fn send_agentless_traces_inner_v1<T: TraceData>(
        &self,
        traces: PooledTraceChunks<'_, T>,
        config: &AgentlessTraceConfig,
        client_side_stats: bool,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_v1_chunks(&traces);
        send_agentless_traces_with_observer_v1(
            &self.capabilities,
            traces,
            &self.metadata,
            config,
            client_side_stats,
            |_result, _payload_len| {
                #[cfg(feature = "telemetry")]
                self.emit_retry_result(_result, _payload_len, counts);
            },
            || {
                #[cfg(feature = "telemetry")]
                self.emit_serialization_drop(counts);
            },
        )
        .await?;
        Ok(AgentResponse::Unchanged)
    }

    /// Sends v1 trace chunks via OTLP HTTP (JSON or protobuf) when OTLP config is enabled.
    async fn send_otlp_traces_inner_v1<T: TraceData>(
        &self,
        traces: &[TraceChunk<T>],
        config: &OtlpTraceConfig,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_v1_chunks(traces);
        let request = map_traces_to_otlp_v1(
            traces,
            &self.otlp_resource_info.load(),
            config.otel_trace_semantics_enabled,
        );
        let body = config
            .protocol
            .encode(&request)
            .ok_or_else(|| {
                TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(
                    "OTLP gRPC protocol cannot be encoded on the HTTP export path".to_string(),
                ))
            })?
            .map_err(|e| {
                error!("OTLP serialization error: {e}");
                #[cfg(feature = "telemetry")]
                self.emit_serialization_drop(counts);
                TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
                    "failed to encode OTLP request: {e}"
                )))
            })?;
        // Also set the header: resource attributes survive Collector hops, headers don't.
        let effective_config;
        let config_to_use = if self.metadata.client_computed_stats || self.otlp_stats_enabled {
            effective_config = {
                let mut c = config.clone();
                c.headers.insert(
                    http::HeaderName::from_static("datadog-client-computed-stats"),
                    http::HeaderValue::from_static("yes"),
                );
                c
            };
            &effective_config
        } else {
            config
        };
        #[cfg(feature = "telemetry")]
        let payload_len = body.len();
        send_otlp_http_with_observer(
            &self.capabilities,
            &config_to_use.endpoint_url,
            "/v1/traces",
            &config_to_use.headers,
            config_to_use.timeout,
            self.endpoint.test_token.as_deref(),
            config_to_use.protocol.content_type().ok_or_else(|| {
                TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(
                    "OTLP gRPC protocol cannot be sent over the HTTP export path".to_string(),
                ))
            })?,
            body,
            OTLP_MAX_RETRIES,
            |_result| {
                #[cfg(feature = "telemetry")]
                self.emit_retry_result(_result, payload_len, counts);
            },
        )
        .await?;
        Ok(AgentResponse::Unchanged)
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn send_otlp_grpc_inner_v1<T: TraceData>(
        &self,
        traces: &[TraceChunk<T>],
        transport: &OtlpGrpcTransport,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_v1_chunks(traces);
        let request = Arc::new(map_traces_to_otlp_v1(
            traces,
            &self.otlp_resource_info.load(),
            transport.otel_trace_semantics_enabled,
        ));
        #[cfg(feature = "telemetry")]
        let payload_len = request.encoded_len() + 5;
        let test_token = self.endpoint.test_token.as_deref();
        let mut attempt: u32 = 1;
        let result = loop {
            match send_otlp_traces_grpc(
                transport,
                test_token,
                self.metadata.client_computed_stats || self.otlp_stats_enabled,
                request.clone(),
            )
            .await
            {
                Ok(()) => break Ok(()),
                Err(GrpcExportError::Retryable { error, retry_after }) => {
                    if attempt > OTLP_MAX_RETRIES {
                        break Err(error);
                    }
                    let Some(delay) = grpc_retry_delay(attempt, retry_after, grpc_retry_jitter())
                    else {
                        break Err(error);
                    };
                    self.capabilities.sleep(delay).await;
                    attempt += 1;
                }
                Err(GrpcExportError::NonRetryable(error)) => break Err(error),
            }
        };
        #[cfg(feature = "telemetry")]
        self.emit_grpc_result(&result, attempt, payload_len, counts);
        result?;
        Ok(AgentResponse::Unchanged)
    }

    /// Synchronous log-export path. Like [`Self::send_trace_chunks_to_log`], this emits every
    /// span as-is, without agent negotiation, stats, OTLP, or telemetry.
    fn send_trace_chunks_to_log_v1<T: TraceData>(
        &self,
        traces: &mut PooledTraceChunks<'_, T>,
        max_line_size: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let mut payload = self.take_into_v1_payload(traces);
        let result = write_log_traces_v1(&self.capabilities, &payload, max_line_size)
            .map_err(TraceExporterError::Io);
        // Restore the chunks even on error so their allocations can be recycled into the pool.
        **traces = std::mem::take(&mut payload.chunks);
        let stats = result?;
        debug!(
            spans_written = stats.spans_written,
            spans_dropped = stats.spans_dropped,
            "Wrote traces to log exporter"
        );
        Ok(AgentResponse::Unchanged)
    }

    async fn process_and_send_trace_chunks_v1<T: TraceData>(
        &self,
        mut traces: PooledTraceChunks<'_, T>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Keep `traces` owned so its spans can be recycled into the pool when it is dropped.
        // Paths that borrow a TracerPayload temporarily take and restore its chunks; consuming
        // paths use `into_chunks()` and forgo pooling.
        //
        // TODO(APMSP-3608): log-output silently takes precedence over OTLP/agent here.
        // The builder should reject conflicting destinations at build time instead.
        if let Some(max_line_size) = self.log_output {
            return self.send_trace_chunks_to_log_v1(&mut traces, max_line_size);
        }

        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Process stats computation and drop non-sampled (p0) chunks.
        // This must run before the OTLP path so that unsampled spans are not exported.
        let client_side_stats = stats::process_traces_for_stats_v1(
            &mut traces,
            &mut header_tags,
            &self.client_side_stats.status,
            self.client_computed_top_level,
            &self.trace_filterer.load(),
            #[cfg(feature = "telemetry")]
            self.telemetry.load_full().as_deref(),
        );

        for chunk in traces.iter_mut() {
            // TODO(paullgdc): this needs to be refactored. We need to have a single enum containing
            // the exporter we use and it's config, and have a more streamlined "pipeline" rather
            // than pepering exporter specific logic...
            //
            // For otlp we don't tag, for the V1 DD exporter this added as a tracer level attribute
            // lower in this function. This is done here before dedup.
            if self.agentless_config.is_some()
                && let Some(span) = chunk.spans.first_mut()
            {
                add_native_export_marker_v1(&mut span.attributes);
            }
            chunk.dedup();
        }

        if let Some(ref config) = self.agentless_config {
            if traces.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            return self
                .send_agentless_traces_inner_v1(traces, config, client_side_stats)
                .await;
        }

        if let Some(otlp) = &self.otlp {
            // Unlike the agent path, there is no downstream agent to drop unsampled traces,
            // if client-side-stats is enabled, we have already dropped chunks after performing stat
            // computations otherwise drop here
            if !client_side_stats {
                trace_utils_v1::drop_chunks(&mut traces);
            }
            if traces.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            return match otlp {
                OtlpExportMode::Http(config) => {
                    self.send_otlp_traces_inner_v1(&traces, config).await
                }
                #[cfg(not(target_arch = "wasm32"))]
                OtlpExportMode::Grpc(transport) => {
                    self.send_otlp_grpc_inner_v1(&traces, transport).await
                }
            };
        }

        self.send_trace_chunks_to_agent_v1(&mut traces, header_tags)
            .await
    }

    /// Serialize processed chunks in the negotiated wire format and send them to the agent.
    ///
    /// Only the wire format negotiates: when v1 is unavailable, native v1 chunks are downgraded
    /// to v0.4 msgpack. This does not turn the pipeline into a general v0.4↔v1 conversion path.
    async fn send_trace_chunks_to_agent_v1<T: TraceData>(
        &self,
        traces: &mut PooledTraceChunks<'_, T>,
        header_tags: TracerHeaderTags<'_>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Snapshot the effective format once so the serializer and the URL agree even if
        // `v1_active` flips mid-send (the background `/info` fetcher can race us otherwise).
        let v1_supported = self.v1_active.load(Ordering::Relaxed);
        let effective_format = if v1_supported {
            TraceExporterOutputFormat::V1
        } else {
            TraceExporterOutputFormat::V04
        };

        let counts = PayloadCounts::from_v1_chunks(traces);
        let headers = self.serializer.build_traces_headers(
            header_tags,
            counts.chunks,
            self.agent_payload_response_version.as_ref(),
        );
        let mut out_payload = self.take_into_v1_payload(traces);
        add_native_export_marker_v1(&mut out_payload.attributes);

        let mp_payload = if v1_supported {
            msgpack_encoder::v1::to_vec_from_v1(&out_payload)
        } else {
            msgpack_encoder::v04::to_vec_from_v1(&out_payload)
        };
        // Give the chunks back to `traces` now that encoding is done, letting its `Drop` recycle
        // them into the pool (if any) instead of dropping them outright.
        **traces = std::mem::take(&mut out_payload.chunks);

        let endpoint = Endpoint {
            url: effective_format.add_path(&self.endpoint.url),
            ..self.endpoint.clone()
        };

        let result = self
            .send_traces_with_telemetry(&endpoint, mp_payload, headers, counts)
            .await;

        // The agent does not return a `Datadog-Agent-State` header on 404, so without this hook
        // we'd stay pinned to V1 until the next `/info` poll (up to the fetcher's refresh
        // interval). On a 404 to `/v1.0/traces`, fail closed immediately and force an `/info`
        // refresh so the next send uses V0.4 and V1 support is re-detected as soon as the agent
        // advertises it again.
        if effective_format == TraceExporterOutputFormat::V1
            && let Err(TraceExporterError::Request(ref e)) = result
            && e.status() == http::StatusCode::NOT_FOUND
            && self.v1_active.swap(false, Ordering::Relaxed)
        {
            warn!(
                "V1 trace send returned 404; agent no longer advertises {V1_TRACES_ENDPOINT} — falling back to V0.4"
            );
            self.info_response_observer.manual_trigger();
        }

        result
    }

    /// Move `traces`' chunks into a fresh [`libdd_trace_utils::span::v1::TracerPayload`] carrying
    /// this exporter's metadata, taking them via `std::mem::take` rather than `into_chunks()` so
    /// the caller can move them back into `traces` after encoding, letting `traces`' `Drop`
    /// recycle them into the pool (if any) instead of dropping them outright.
    fn take_into_v1_payload<T: TraceData>(
        &self,
        traces: &mut PooledTraceChunks<'_, T>,
    ) -> libdd_trace_utils::span::v1::TracerPayload<T> {
        libdd_trace_utils::span::v1::TracerPayload {
            container_id: T::Text::from_owned(self.metadata.container_id.clone()),
            language_name: T::Text::from_owned(self.metadata.language.clone()),
            language_version: T::Text::from_owned(self.metadata.language_version.clone()),
            tracer_version: T::Text::from_owned(self.metadata.tracer_version.clone()),
            runtime_id: T::Text::from_owned(
                self.metadata.mutable_metadata.load().runtime_id.clone(),
            ),
            env: T::Text::from_owned(self.metadata.env.clone()),
            hostname: T::Text::from_owned(self.metadata.hostname.clone()),
            app_version: T::Text::from_owned(self.metadata.app_version.clone()),
            attributes: Default::default(),
            chunks: std::mem::take(&mut **traces),
        }
    }
}

/// V1 counterpart of [`super::v04::add_native_export_marker`]
fn add_native_export_marker_v1<T: TraceData>(attributes: &mut VecMap<T::Text, AttributeValue<T>>) {
    attributes.insert(
        T::Text::from_static_str(SDK_OTLP_EXPORT_KEY),
        AttributeValue::String(T::Text::from_static_str("false")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_info;
    use crate::trace_exporter::tests::{CapturingCapabilities, build_test_exporter, captured_log};
    use crate::trace_exporter::{INFO_ENDPOINT, TraceExporterBuilder, TraceExporterInputFormat};
    use httpmock::MockServer;
    use httpmock::prelude::*;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_tinybytes::BytesString;
    use std::time::Duration;

    // V1-native counterpart of `test_log_mode_send_writes_forwarder_json`: sends native v1
    // chunks, hits the log branch in `process_and_send_trace_chunks_v1`, and writes
    // Forwarder-format JSON bytes through the log-output capability.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_log_mode_send_v1_writes_forwarder_json() {
        use libdd_trace_utils::span::v1::{SpanBytes as SpanBytesV1, TraceChunkBytes};

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("test")
            .set_output_format(TraceExporterOutputFormat::V1)
            .set_output_to_log(None);
        let exporter = builder.build::<CapturingCapabilities>().unwrap();

        let chunks = vec![TraceChunkBytes {
            trace_id: [0; 16],
            spans: vec![SpanBytesV1 {
                name: BytesString::from_slice(b"aws.lambda").unwrap(),
                span_id: 2,
                ..Default::default()
            }],
            ..Default::default()
        }];

        let pool = libdd_trace_utils::span::v1::span_pool::SpanPoolV1::with_capacity(100);
        let resp = exporter
            .send_trace_chunks_v1(pool.wrap_chunks(chunks), None)
            .unwrap();
        assert!(matches!(resp, AgentResponse::Unchanged));
        assert!(!pool.is_empty(), "log export must recycle the sent chunks");

        let text = String::from_utf8(captured_log()).unwrap();
        assert!(text.ends_with('\n'), "line must be newline-terminated");
        let line = text.trim_end();
        let v: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        assert!(v["traces"][0][0]["trace_id"].is_string());
        assert_eq!(v["traces"][0][0]["span_id"], "0000000000000002");
        assert_eq!(v["traces"][0][0]["name"], "aws.lambda");
    }

    /// V1-native counterpart of `test_agentless_export_via_builder`: sends native v1 chunks
    /// through the agentless JSON dispatch path.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_via_builder_v1() {
        use libdd_trace_utils::span::v1::{SpanBytes as SpanBytesV1, TraceChunkBytes};

        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/input")
                .header("Content-Type", "application/json")
                .header("dd-api-key", "test-api-key")
                .header("X-Datadog-Trace-Count", "1")
                .header("datadog-meta-lang", "nodejs")
                .header("datadog-meta-tracer-version", "1.0");
            then.status(200).body("");
        });

        let intake_url = format!("{}/v1/input", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("nodejs")
            .set_language_version("v20.11.0")
            .set_language_interpreter("v8")
            .set_agentless_endpoint(&intake_url, "test-api-key")
            .set_output_format(TraceExporterOutputFormat::V1);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let chunks = vec![TraceChunkBytes {
            trace_id: [0; 16],
            spans: vec![SpanBytesV1 {
                name: BytesString::from_slice(b"op").unwrap(),
                service: BytesString::from_static("svc"),
                resource: BytesString::from_static("res"),
                span_id: 2,
                parent_id: 0,
                start: 2_500_000_000,
                duration: 1_000_000,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let result = exporter.send_trace_chunks_v1(PooledTraceChunks::unpooled(chunks), None);

        assert!(
            result.is_ok(),
            "V1 agentless send should succeed: {:?}",
            result.err()
        );
        mock_intake.assert();
        assert_eq!(mock_intake.calls(), 1);
    }

    /// `send_trace_chunks_v1` is the sync facade over `send_trace_chunks_async_v1`, sending
    /// native v1 chunks.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_trace_chunks_v1_sync() {
        use libdd_trace_utils::span::v1::{SpanBytes as SpanBytesV1, TraceChunkBytes};

        agent_info::clear_cache_for_test();

        let server = MockServer::start();
        let mock_traces = server.mock(|when, then| {
            when.method(POST).path("/v1.0/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{ "rate_by_service": { "service:,env:": 1.0 } }"#);
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(format!(
                    r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V1_TRACES_ENDPOINT}"]}}"#
                ));
        });

        let exporter = build_test_exporter(
            server.url("/"),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );

        let start = std::time::Instant::now();
        while agent_info::get_agent_info().is_none() {
            if start.elapsed() > Duration::from_secs(5) {
                panic!("timeout waiting for /info");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let chunks = vec![TraceChunkBytes {
            trace_id: [0; 16],
            spans: vec![SpanBytesV1 {
                name: BytesString::from_slice(b"op").unwrap(),
                service: BytesString::from_static("svc"),
                resource: BytesString::from_static("res"),
                span_id: 2,
                parent_id: 0,
                start: 0,
                duration: 1,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let result = exporter.send_trace_chunks_v1(PooledTraceChunks::unpooled(chunks), None);

        assert!(
            result.is_ok(),
            "sync v1 native send should succeed: {:?}",
            result.err()
        );
        mock_traces.assert();
        assert_eq!(mock_traces.calls(), 1);
    }

    /// A full v1 send through the agent path recycles the sent chunk back into the pool.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_trace_chunks_v1_feeds_a_real_pool() {
        use libdd_trace_utils::span::v1::span_pool::SpanPoolV1;
        use libdd_trace_utils::span::v1::{SpanBytes as SpanBytesV1, TraceChunkBytes};

        agent_info::clear_cache_for_test();

        let server = MockServer::start();
        let mock_traces = server.mock(|when, then| {
            when.method(POST).path("/v1.0/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{ "rate_by_service": { "service:,env:": 1.0 } }"#);
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(format!(
                    r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V1_TRACES_ENDPOINT}"]}}"#
                ));
        });

        let exporter = build_test_exporter(
            server.url("/"),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );

        let start = std::time::Instant::now();
        while agent_info::get_agent_info().is_none() {
            if start.elapsed() > Duration::from_secs(5) {
                panic!("timeout waiting for /info");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let chunks = vec![TraceChunkBytes {
            trace_id: [0; 16],
            spans: vec![SpanBytesV1 {
                name: BytesString::from_slice(b"op").unwrap(),
                service: BytesString::from_static("svc"),
                resource: BytesString::from_static("res"),
                span_id: 2,
                parent_id: 0,
                start: 0,
                duration: 1,
                ..Default::default()
            }],
            ..Default::default()
        }];

        let pool = SpanPoolV1::with_capacity(100);
        assert!(pool.is_empty(), "pool should start empty");
        let result = exporter.send_trace_chunks_v1(pool.wrap_chunks(chunks), None);

        assert!(
            result.is_ok(),
            "sync v1 native send should succeed: {:?}",
            result.err()
        );
        mock_traces.assert();
        assert_eq!(mock_traces.calls(), 1);
        assert!(
            !pool.is_empty(),
            "the sent chunk should have been recycled back into the pool"
        );
    }
}
