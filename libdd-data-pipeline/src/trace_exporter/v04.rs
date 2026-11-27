// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! TraceExporter methods for the v0.4 span model.

use super::agent_response::AgentResponse;
use super::error::{self, InternalErrorKind, TraceExporterError};
use super::log_writer::write_log_traces;
use super::{
    DeserInputFormat, OtlpExportMode, PayloadCounts, TraceExporter, TraceExporterOutputFormat,
    V1_TRACES_ENDPOINT, stats,
};
#[cfg(not(target_arch = "wasm32"))]
use super::{grpc_retry_delay, grpc_retry_jitter};
use crate::agentless::AgentlessTraceConfig;
use crate::agentless::exporter::send_agentless_traces_with_observer;
use crate::health_metrics::{self, HealthMetric};
use crate::otlp::exporter::{OTLP_MAX_RETRIES, send_otlp_http_with_observer};
#[cfg(not(target_arch = "wasm32"))]
use crate::otlp::{GrpcExportError, OtlpGrpcTransport, send_otlp_traces_grpc};
use crate::otlp::{OtlpTraceConfig, map_traces_to_otlp};
use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_common::Endpoint;
#[cfg(not(target_arch = "wasm32"))]
use libdd_shared_runtime::BlockingRuntime;
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::span::span_pool::PooledChunks;
use libdd_trace_utils::span::trace_utils::SDK_OTLP_EXPORT_KEY;
use libdd_trace_utils::span::v04::{Span, SpanSlice};
use libdd_trace_utils::span::{SpanText, TraceData};
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
    fn from_traces<T: TraceData>(traces: &[Vec<Span<T>>]) -> Self {
        Self {
            chunks: traces.len(),
            #[cfg(feature = "telemetry")]
            spans: traces.iter().map(Vec::len).sum(),
        }
    }
}

impl<
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> TraceExporter<C, R>
{
    /// Send msgpack serialized traces to the agent.
    ///
    /// Sync facade over [`Self::send_async`]; panics inside an existing tokio context.
    /// `data` must be encoded per the `input_format` given to the builder. Returns the
    /// agent response on success.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send(&self, data: &[u8]) -> Result<AgentResponse, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        self.shared_runtime.block_on(self.send_async(data))?
    }

    /// Send msgpack serialized traces to the agent.
    ///
    /// `data` must be encoded per the `input_format` given to the builder.
    /// [`Self::send`] is the sync facade over this method.
    pub async fn send_async(&self, data: &[u8]) -> Result<AgentResponse, TraceExporterError> {
        // There is no agent to negotiate with, skip the poll.
        if self.log_output.is_none() && self.agentless_config.is_none() {
            self.check_agent_info().await;
        }

        let traces = self.deserialize_traces(data, self.input_format.into())?;

        let res = self
            .process_and_send_trace_chunks(PooledChunks::unpooled(traces))
            .await?;
        if matches!(&res, AgentResponse::Changed { body } if body.is_empty()) {
            return Err(TraceExporterError::Agent(
                error::AgentErrorKind::EmptyResponse,
            ));
        }
        Ok(res)
    }

    fn deserialize_traces<'a>(
        &self,
        data: &'a [u8],
        format: DeserInputFormat,
    ) -> Result<Vec<Vec<SpanSlice<'a>>>, TraceExporterError> {
        let (traces, _) = match format {
            DeserInputFormat::V04 => msgpack_decoder::v04::from_slice(data),
            DeserInputFormat::V05 => msgpack_decoder::v05::from_slice(data),
        }
        .map_err(|e| {
            error!("Error deserializing trace from request body: {e}");
            self.emit_metric(
                HealthMetric::Count(health_metrics::DESERIALIZE_TRACES_ERRORS, 1),
                None,
            );
            TraceExporterError::Deserialization(e)
        })?;
        debug!(
            trace_count = traces.len(),
            "Trace deserialization completed successfully"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::DESERIALIZE_TRACES, traces.len() as i64),
            None,
        );
        Ok(traces)
    }

    /// Send a list of trace chunks to the agent (or OTLP endpoint when configured).
    ///
    /// Sync facade over [`Self::send_trace_chunks_async`]; panics inside an existing
    /// tokio context.
    ///
    /// # Arguments
    /// * trace_chunks: A list of trace chunks. Each trace chunk is a list of spans.
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
    pub fn send_trace_chunks<T: TraceData>(
        &self,
        trace_chunks: PooledChunks<'_, T>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<AgentResponse, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        self.shared_runtime
            .block_on(self.send_trace_chunks_cancellable(trace_chunks, cancellation_token))?
    }

    /// Send chunks, giving up with an `Interrupted` error once the token is cancelled.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) async fn send_trace_chunks_cancellable<T: TraceData>(
        &self,
        trace_chunks: PooledChunks<'_, T>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<AgentResponse, TraceExporterError> {
        match cancellation_token {
            Some(token) => {
                tokio::select! {
                    res = self.send_trace_chunks_async(trace_chunks) => res,
                    _ = token.cancelled() => Err(TraceExporterError::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "send cancelled via cancellation token",
                    ))),
                }
            }
            None => self.send_trace_chunks_async(trace_chunks).await,
        }
    }

    /// Send a list of trace chunks to the agent, asynchronously (or OTLP when configured).
    ///
    /// # Arguments
    /// * trace_chunks: A list of trace chunks. Each trace chunk is a list of spans.
    ///
    /// # Returns
    /// * Ok(AgentResponse): The response from the agent (or Unchanged for OTLP)
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    pub async fn send_trace_chunks_async<T: TraceData>(
        &self,
        trace_chunks: PooledChunks<'_, T>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // There is no agent to negotiate with, skip the poll.
        if self.log_output.is_none() && self.agentless_config.is_none() {
            self.check_agent_info().await;
        }
        self.process_and_send_trace_chunks(trace_chunks).await
    }

    /// Sends trace chunks to the Datadog agentless intake (`/v1/input`) as JSON.
    async fn send_agentless_traces_inner<T: TraceData>(
        &self,
        traces: PooledChunks<'_, T>,
        config: &AgentlessTraceConfig,
        client_side_stats: bool,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_traces(&traces);
        send_agentless_traces_with_observer(
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

    /// Sends trace chunks via OTLP HTTP (JSON or protobuf) when OTLP config is enabled.
    async fn send_otlp_traces_inner<T: TraceData>(
        &self,
        traces: &[Vec<Span<T>>],
        config: &OtlpTraceConfig,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_traces(traces);
        let request = map_traces_to_otlp(
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
    async fn send_otlp_grpc_inner<T: TraceData>(
        &self,
        traces: &[Vec<Span<T>>],
        transport: &OtlpGrpcTransport,
    ) -> Result<AgentResponse, TraceExporterError> {
        #[cfg(feature = "telemetry")]
        let counts = PayloadCounts::from_traces(traces);
        let request = Arc::new(map_traces_to_otlp(
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

    /// Synchronous log-export path: encode every span to newline-delimited
    /// Forwarder JSON and write it through the log-output capability (stdout on
    /// native; host/JS on wasm). No agent, stats, OTLP, or telemetry is involved.
    ///
    /// Unlike the OTLP path, spans are emitted as-is and unsampled (p0) chunks are
    /// NOT dropped here: the reference log exporters (JS/Go/Py/Java) write every
    /// span they are handed and defer sampling to the trace intake behind the
    /// Datadog Forwarder.
    ///
    /// Returns [`AgentResponse::Unchanged`] as there is no agent response to relay.
    fn send_trace_chunks_to_log<T: TraceData>(
        &self,
        traces: &[Vec<Span<T>>],
        max_line_size: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let stats = write_log_traces(&self.capabilities, traces, max_line_size)
            .map_err(TraceExporterError::Io)?;
        debug!(
            spans_written = stats.spans_written,
            spans_dropped = stats.spans_dropped,
            "Wrote traces to log exporter"
        );
        Ok(AgentResponse::Unchanged)
    }

    async fn process_and_send_trace_chunks<T: TraceData>(
        &self,
        mut traces: PooledChunks<'_, T>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // `traces` is a `PooledChunks`: keeping it owned (rather than moving its inner `Vec`
        // into the consuming code paths) is what lets its spans be recycled into the pool when
        // it is dropped at the end of this function. Paths that must consume the spans use
        // `into_chunks()` and forgo pooling.
        //
        // TODO(APMSP-3608): log-output silently takes precedence over OTLP/agent here.
        // The builder should reject conflicting destinations at build time instead.
        if let Some(max_line_size) = self.log_output {
            return self.send_trace_chunks_to_log(&traces, max_line_size);
        }

        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Process stats computation and drop non-sampled (p0) chunks.
        // This must run before the OTLP path so that unsampled spans are not exported.
        let client_side_stats = stats::process_traces_for_stats(
            &mut traces,
            &mut header_tags,
            &self.client_side_stats.status,
            self.client_computed_top_level,
            &self.trace_filterer.load(),
            #[cfg(feature = "telemetry")]
            self.telemetry.load_full().as_deref(),
        );

        // Agent and agentless both send the Datadog format; the builder rejects agentless + OTLP.
        let native_export = self.otlp.is_none();
        for chunk in traces.iter_mut() {
            if native_export && let Some(first) = chunk.first_mut() {
                add_native_export_marker(first);
            }
            for span in chunk.iter_mut() {
                span.dedup();
            }
        }

        if let Some(ref config) = self.agentless_config {
            if traces.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            return self
                .send_agentless_traces_inner(traces, config, client_side_stats)
                .await;
        }

        if let Some(otlp) = &self.otlp {
            // Unlike the agent path, there is no downstream agent to drop unsampled traces,
            // if client-side-stats is enabled, we have already dropped chunks after performing stat
            // computations otherwise drop here
            if !client_side_stats {
                libdd_trace_utils::span::trace_utils::drop_chunks(&mut traces);
            }
            if traces.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            return match otlp {
                OtlpExportMode::Http(config) => self.send_otlp_traces_inner(&traces, config).await,
                #[cfg(not(target_arch = "wasm32"))]
                OtlpExportMode::Grpc(transport) => {
                    self.send_otlp_grpc_inner(&traces, transport).await
                }
            };
        }

        self.send_trace_chunks_to_agent(&traces, header_tags).await
    }

    /// Serialize processed chunks in the negotiated wire format and send them to the agent.
    async fn send_trace_chunks_to_agent<T: TraceData>(
        &self,
        traces: &[Vec<Span<T>>],
        header_tags: TracerHeaderTags<'_>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Snapshot the effective format once so the serializer and the URL agree even if
        // `v1_active` flips mid-send (the background `/info` fetcher can race us otherwise).
        let effective_format = self.effective_output_format();
        let counts = PayloadCounts::from_traces(traces);

        let prepared = match self.serializer.prepare_traces_payload(
            traces,
            header_tags,
            &self.metadata,
            self.agent_payload_response_version.as_ref(),
            effective_format,
        ) {
            Ok(p) => p,
            Err(e) => {
                error!("Error serializing traces: {e}");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::SERIALIZE_TRACES_ERRORS, 1),
                    None,
                );
                #[cfg(feature = "telemetry")]
                self.emit_serialization_drop(counts);
                return Err(e);
            }
        };
        let counts = PayloadCounts {
            chunks: prepared.chunk_count,
            ..counts
        };

        let endpoint = Endpoint {
            url: effective_format.add_path(&self.endpoint.url),
            ..self.endpoint.clone()
        };

        let result = self
            .send_traces_with_telemetry(&endpoint, prepared.data, prepared.headers, counts)
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
}

/// Marks a span as exported natively (not over OTLP) with `_dd.sdk.otlp_export: "false"`.
pub(super) fn add_native_export_marker<T: TraceData>(span: &mut Span<T>) {
    span.meta.insert(
        T::Text::from_static_str(SDK_OTLP_EXPORT_KEY),
        T::Text::from_static_str("false"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::tests::{CapturingCapabilities, captured_log};
    use crate::trace_exporter::{
        TraceExporterBuilder, TraceExporterInputFormat, V04_TRACES_ENDPOINT,
    };
    use httpmock::MockServer;
    use httpmock::prelude::*;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::ForkSafeRuntime;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::msgpack_encoder;
    use libdd_trace_utils::span::v04::SpanBytes;
    use std::collections::BTreeMap;

    // The real `send` entry point decodes msgpack, hits the log branch, and writes
    // Forwarder-format JSON bytes through the log-output capability.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_log_mode_send_writes_forwarder_json() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("test")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_to_log(None);
        let exporter = builder.build::<CapturingCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"aws.lambda").unwrap(),
            trace_id: 1,
            span_id: 2,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);

        let resp = exporter.send(data.as_ref()).unwrap();
        assert!(matches!(resp, AgentResponse::Unchanged));

        let text = String::from_utf8(captured_log()).unwrap();
        assert!(text.ends_with('\n'), "line must be newline-terminated");
        let line = text.trim_end();
        let v: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        // Forwarder is_trace contract + the span actually round-tripped through
        // msgpack decode -> log encode with hex ids.
        assert!(v["traces"][0][0]["trace_id"].is_string());
        assert_eq!(v["traces"][0][0]["span_id"], "0000000000000002");
        assert_eq!(v["traces"][0][0]["name"], "aws.lambda");
    }

    // Log mode must make zero agent HTTP calls — also guards the worker-gating fix
    // (info-fetcher is not spawned in log mode).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_log_mode_makes_no_agent_requests() {
        let fake_agent = MockServer::start();
        // No `when` constraints => matches any request to any path.
        let any = fake_agent.mock(|_when, then| {
            then.status(200).body("{}");
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&fake_agent.url("/"))
            .set_service("test")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_to_log(None);
        let exporter = builder.build::<CapturingCapabilities>().unwrap();
        // Structural guarantee: no agent-info worker is spawned in log mode.
        assert!(exporter.workers.info_fetcher.is_none());

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        // `send` is synchronous and, in log mode, returns after writing through the
        // capability without initiating any HTTP; combined with the structural assert
        // above this is deterministic (no background worker can race the mock).
        exporter.send(data.as_ref()).unwrap();

        assert_eq!(any.calls(), 0, "log mode must not contact the agent");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_otlp_export_via_builder() {
        let server = MockServer::start();
        let mock_otlp = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/traces")
                .header("Content-Type", "application/json");
            then.status(200).body("");
        });

        let otlp_endpoint = format!("{}/v1/traces", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("http://127.0.0.1:8126")
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("rust")
            .set_language_version("1.0")
            .set_language_interpreter("rustc")
            .set_otlp_endpoint(&otlp_endpoint)
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 1,
            span_id: 2,
            parent_id: 0,
            start: 1000,
            duration: 100,
            error: 0,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "OTLP send should succeed: {:?}",
            result.err()
        );
        mock_otlp.assert();
    }

    #[test]
    fn test_add_native_export_marker() {
        let mut span = SpanBytes::default();
        add_native_export_marker(&mut span);
        assert_eq!(
            span.meta.get(SDK_OTLP_EXPORT_KEY).map(|v| v.as_str()),
            Some("false")
        );

        // A marker already set by the tracer is overridden once the span is deduped.
        let mut span = SpanBytes::default();
        span.meta.insert(
            BytesString::from_static(SDK_OTLP_EXPORT_KEY),
            BytesString::from_static("true"),
        );
        add_native_export_marker(&mut span);
        span.dedup();
        assert_eq!(span.meta.len(), 1);
        assert_eq!(
            span.meta.get(SDK_OTLP_EXPORT_KEY).map(|v| v.as_str()),
            Some("false")
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_export_marks_first_span_of_each_chunk() {
        let server = MockServer::start();
        let mock_agent = server.mock(|when, then| {
            when.method(POST).path(V04_TRACES_ENDPOINT).is_true(|req| {
                let Ok((chunks, _)) = msgpack_decoder::v04::from_slice(req.body_ref()) else {
                    return false;
                };
                let is_marked = |span: &libdd_trace_utils::span::v04::SpanSlice| {
                    span.meta
                        .get(SDK_OTLP_EXPORT_KEY)
                        .is_some_and(|v| v == "false")
                };
                chunks.len() == 2
                    && chunks[0].len() == 2
                    && chunks[1].len() == 1
                    && is_marked(&chunks[0][0])
                    && !chunks[0][1].meta.contains_key(SDK_OTLP_EXPORT_KEY)
                    && is_marked(&chunks[1][0])
                    && chunks
                        .iter()
                        .flatten()
                        .all(|span| !span.meta.contains_key("team"))
            });
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"rate_by_service": {}}"#);
        });

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("svc")
            .set_tracer_version("1.0")
            .set_language("rust")
            .set_language_version("1.0")
            .set_language_interpreter("rustc")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        exporter.set_otlp_resource_attributes(BTreeMap::from([(
            "team".into(),
            "resource-only".into(),
        )]));

        let span = |trace_id, span_id, parent_id| SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            trace_id,
            span_id,
            parent_id,
            ..Default::default()
        };
        let traces: Vec<Vec<SpanBytes>> =
            vec![vec![span(1, 1, 0), span(1, 2, 1)], vec![span(2, 3, 0)]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "Agent send should succeed: {:?}",
            result.err()
        );
        mock_agent.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_via_builder() {
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
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 0xdeadbeef,
            span_id: 2,
            parent_id: 0,
            start: 2_500_000_000,
            duration: 1_000_000,
            error: 0,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "Agentless send should succeed: {:?}",
            result.err()
        );
        mock_intake.assert();

        assert_eq!(mock_intake.calls(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_body_shape() {
        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            fn check_body(body: &str) -> bool {
                body.contains("\"traces\":")
                    && body.contains("\"spans\":")
                    && body.contains("\"hostname\":\"h-1\"")
                    && body.contains("\"languageName\":\"nodejs\"")
                    && body.contains("\"_dd.compute_stats\":\"1\"")
                    && body.contains("\"_top_level\":1")
                    && body.contains("\"_trace_root\":1")
                    && body.contains("\"parent_id\":\"0000000000000000\"")
            }
            let when = when.method(POST).path("/v1/input");
            #[cfg(feature = "compression")]
            let when = when.header("content-encoding", "zstd").is_true(|req| {
                #[cfg(not(target_arch = "wasm32"))]
                let body = zstd::decode_all(req.body_ref());
                #[cfg(target_arch = "wasm32")]
                let body = zrip::decompress(req.body_ref());
                let Ok(body) = body else {
                    return false;
                };
                let body = String::from_utf8(body).unwrap();
                check_body(&body)
            });
            #[cfg(not(feature = "compression"))]
            let when = when.is_true(|req| {
                let body = String::from_utf8(req.body_vec()).unwrap();
                check_body(&body)
            });
            let _ = when;
            then.status(200).body("");
        });

        let intake_url = format!("{}/v1/input", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_hostname("h-1")
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("nodejs")
            .set_language_version("v20.11.0")
            .set_language_interpreter("v8")
            .set_agentless_endpoint(&intake_url, "k")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 1,
            span_id: 2,
            parent_id: 0,
            start: 0,
            duration: 1,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        exporter.send(data.as_ref()).unwrap();
        mock_intake.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_marks_first_span_of_each_chunk() {
        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            fn check_body(body: &[u8]) -> bool {
                let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
                    return false;
                };
                let marker = |trace: usize, span: usize| {
                    body["traces"][trace]["spans"][span]["meta"][SDK_OTLP_EXPORT_KEY].as_str()
                };
                body["traces"].as_array().map(Vec::len) == Some(2)
                    && marker(0, 0) == Some("false")
                    && !body["traces"][0]["spans"][1].is_null()
                    && marker(0, 1).is_none()
                    && marker(1, 0) == Some("false")
            }
            let when = when.method(POST).path("/v1/input");
            #[cfg(feature = "compression")]
            let when = when.is_true(|req| {
                #[cfg(not(target_arch = "wasm32"))]
                let body = zstd::decode_all(req.body_ref());
                #[cfg(target_arch = "wasm32")]
                let body = zrip::decompress(req.body_ref());
                body.is_ok_and(|body| check_body(&body))
            });
            #[cfg(not(feature = "compression"))]
            let when = when.is_true(|req| check_body(req.body_ref()));
            let _ = when;
            then.status(200).body("");
        });

        let intake_url = format!("{}/v1/input", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("svc")
            .set_tracer_version("1.0")
            .set_language("rust")
            .set_language_version("1.0")
            .set_language_interpreter("rustc")
            .set_agentless_endpoint(&intake_url, "k")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let span = |trace_id, span_id, parent_id| SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            trace_id,
            span_id,
            parent_id,
            ..Default::default()
        };
        let traces: Vec<Vec<SpanBytes>> =
            vec![vec![span(1, 1, 0), span(1, 2, 1)], vec![span(2, 3, 0)]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "Agentless send should succeed: {:?}",
            result.err()
        );
        mock_intake.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_obfuscates_every_exported_span() {
        // Obfuscation runs in the agentless path on every span that is sent to the
        // intake. With `remove_query_string` enabled, the `secret=bar` query parameter
        // must be stripped from `http.url` before the payload is POSTed.
        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            fn body_is_obfuscated(body: &str) -> bool {
                !body.contains("secret=bar") && body.contains("http://foo.com/path?")
            }
            let when = when.method(POST).path("/v1/input");
            #[cfg(feature = "compression")]
            let when = when.header("content-encoding", "zstd").is_true(|req| {
                #[cfg(not(target_arch = "wasm32"))]
                let body = zstd::decode_all(req.body_ref());
                #[cfg(target_arch = "wasm32")]
                let body = zrip::decompress(req.body_ref());
                let Ok(body) = body else {
                    return false;
                };
                body_is_obfuscated(&String::from_utf8(body).unwrap())
            });
            #[cfg(not(feature = "compression"))]
            let when =
                when.is_true(|req| body_is_obfuscated(&String::from_utf8(req.body_vec()).unwrap()));
            let _ = when;
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
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_span_obfuscation_config({
                let mut cfg =
                    libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig::default();
                cfg.http.remove_query_string = true;
                cfg
            });
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let mut span = SpanBytes {
            name: BytesString::from_slice(b"http.request").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("GET /path"),
            r#type: BytesString::from_static("http"),
            trace_id: 0xdead_beef,
            span_id: 2,
            parent_id: 0,
            start: 2_500_000_000,
            duration: 1_000_000,
            error: 0,
            ..Default::default()
        };
        span.meta.insert(
            BytesString::from_static("http.url"),
            BytesString::from_static("http://foo.com/path?secret=bar"),
        );
        span.dedup();
        let traces: Vec<Vec<SpanBytes>> = vec![vec![span]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());
        assert!(
            result.is_ok(),
            "Agentless send should succeed: {:?}",
            result.err()
        );
        // The mock only matches when the body is obfuscated, so a successful call here
        // proves the query string was stripped before the request was sent.
        mock_intake.assert();
    }
}
