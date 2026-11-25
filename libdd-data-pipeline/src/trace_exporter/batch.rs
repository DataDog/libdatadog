// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Bounded Agent sends with explicit operation state. Each call owns its results;
//! nested sends cannot change its framing, observations, or cancellation counters.

use super::{
    AgentResponse, PayloadCounts, TraceExporter, TraceExporterError, TraceExporterOutputFormat,
    error::BuilderErrorKind,
    observations::TraceExporterObservations,
    payloads::{DEFAULT_MAX_PAYLOAD_BYTES, Payload, Payloads},
};
use crate::{health_metrics::SendResult, telemetry::SendPayloadTelemetry};
use libdd_capabilities::{
    Bytes, HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability,
};
use libdd_common::Endpoint;
use libdd_shared_runtime::{BlockingRuntime, SharedRuntime};
use libdd_trace_utils::{
    send_with_retry::{
        BoundedResponse, CompressionStrategy, RetryStrategy, SendWithRetryError,
        SendWithRetryResult, send_with_bounded_response,
    },
    span::{TraceData, span_pool::PooledChunks},
};
use std::{
    io,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const MAX_FEEDBACK_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

struct ResponseState<F> {
    interrupted: F,
    latest_feedback: Option<usize>,
    diagnostic_bytes_remaining: usize,
}

struct RatesObject;

impl<'de> serde::Deserialize<'de> for RatesObject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
            type Value = RatesObject;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a sampling rates object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<RatesObject, M::Error> {
                while let Some((_, rate)) = map.next_entry::<String, f64>()? {
                    if !(0.0..=1.0).contains(&rate) {
                        return Err(serde::de::Error::custom("sampling rate outside [0, 1]"));
                    }
                }
                Ok(RatesObject)
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

#[derive(serde::Deserialize)]
struct FeedbackShape<'a> {
    #[serde(borrow)]
    rate_by_service: &'a serde_json::value::RawValue,
}

fn sampling_feedback(body: &[u8]) -> Option<String> {
    if body.len() > MAX_FEEDBACK_BYTES {
        return None;
    }
    let body = std::str::from_utf8(body).ok()?;
    if !body.trim_start().starts_with('{') {
        return None;
    }
    let rates = serde_json::from_str::<FeedbackShape>(body)
        .ok()?
        .rate_by_service
        .get();
    serde_json::from_str::<RatesObject>(rates).ok()?;
    let prefix = r#"{"rate_by_service":"#;
    let mut body = String::with_capacity(prefix.len() + rates.len() + 1);
    body.push_str(prefix);
    body.push_str(rates);
    body.push('}');
    Some(body)
}

/// One payload outcome, with its own counters. Local serialisation failures have
/// zero requests. Only the latest usable sampling response in a batch retains a body.
#[derive(Debug)]
pub struct PayloadResult {
    pub chunks: usize,
    pub spans: usize,
    pub response: Result<AgentResponse, TraceExporterError>,
    pub observations: TraceExporterObservations,
}

/// An operation error means processing did not complete, not that all payloads
/// failed. Inspect individual outcomes and local drops independently. In-flight
/// delivery is unconfirmed; unattempted chunks have not been dispatched.
#[derive(Debug, Default)]
pub struct BatchResult {
    pub payloads: Vec<PayloadResult>,
    pub oversized_chunks: usize,
    pub oversized_spans: usize,
    pub unattempted_chunks: usize,
    pub unattempted_spans: usize,
    pub in_flight_chunks: usize,
    pub in_flight_spans: usize,
    /// Preprocessing counts not assigned to a completed send attempt. These are
    /// not delivery acknowledgements: cancellation may leave an attempted send
    /// uncertain, and a completed attempt may fail. Build failures retain them.
    pub pending_p0_traces: u64,
    pub pending_p0_spans: u64,
    pub error: Option<TraceExporterError>,
}

fn observations(
    result: &SendWithRetryResult,
    bytes: usize,
    counts: PayloadCounts,
) -> TraceExporterObservations {
    let mut report = TraceExporterObservations::default();
    report.set_payload(SendPayloadTelemetry::from_retry_result_with_spans(
        result,
        u64::try_from(bytes).unwrap_or(u64::MAX),
        u64::try_from(counts.chunks).unwrap_or(u64::MAX),
        u64::try_from(counts.spans).unwrap_or(u64::MAX),
    ));
    report
}

impl<
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> TraceExporter<C, R>
{
    /// Bounded Agent export with caller-owned telemetry delivery. Zero selects a
    /// 5 MiB cap, lowered by endpoint discovery. This call does not submit payload
    /// observations through native telemetry. It requests full sampling feedback
    /// without advancing the legacy response-version cache.
    ///
    /// The operation-wide elapsed-time budget is the default strategy's attempt
    /// count times the request timeout, plus ten seconds. Each payload retains
    /// its own retry allowance. The byte limit applies to each encoded request.
    /// A candidate is encoded in full before splitting: its temporary allocation can exceed
    /// the limit, and cancellation or deadline expiry cannot interrupt an encode in progress.
    /// Discovery reconciliation can also wait beyond this budget for the stats worker's final
    /// flush.
    pub fn send_trace_chunks_batched<T: TraceData>(
        &self,
        chunks: PooledChunks<'_, T>,
        max_payload_bytes: usize,
        cancel: Option<&CancellationToken>,
    ) -> BatchResult
    where
        R: BlockingRuntime,
    {
        let counts = PayloadCounts::from_traces(&chunks);
        let mut result = BatchResult {
            unattempted_chunks: counts.chunks,
            unattempted_spans: counts.spans,
            ..Default::default()
        };
        let budget = Duration::from_millis(self.endpoint.timeout_ms)
            .saturating_mul(RetryStrategy::default().max_retries().saturating_add(1))
            .saturating_add(Duration::from_secs(10));
        let Some(deadline) = Instant::now().checked_add(budget) else {
            result.error = Some(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration("batch timeout is too large".to_owned()),
            ));
            return result;
        };
        let never_cancelled = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never_cancelled);
        let outcome = self
            .shared_runtime
            .block_on(self.send_batch(chunks, max_payload_bytes, cancel, deadline, &mut result))
            .map_err(TraceExporterError::from)
            .and_then(|outcome| outcome);
        result.error = outcome.err();
        result
    }

    async fn send_batch<T: TraceData>(
        &self,
        mut traces: PooledChunks<'_, T>,
        local_limit: usize,
        cancel: &CancellationToken,
        deadline: Instant,
        result: &mut BatchResult,
    ) -> Result<(), TraceExporterError> {
        if self.log_output.is_some() || self.agentless_config.is_some() || self.otlp.is_some() {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "bounded batch export requires an Agent destination".to_owned(),
                ),
            ));
        }
        let interrupted = async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => io::ErrorKind::Interrupted,
                _ = self.capabilities.sleep(deadline.saturating_duration_since(Instant::now())) => io::ErrorKind::TimedOut,
            }
        };
        tokio::pin!(interrupted);
        let check_stop = || {
            if cancel.is_cancelled() {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else if Instant::now() >= deadline {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            } else {
                Ok(())
            }
        };
        check_stop()?;
        // Removing a stats worker transfers responsibility for its final flush.
        self.check_agent_info().await;
        check_stop()?;
        let (mut headers, _) = self.prepare_trace_chunks(&mut traces);
        result.unattempted_chunks = traces.len();
        result.unattempted_spans = traces.iter().map(Vec::len).sum();
        result.pending_p0_traces =
            u64::try_from(headers.generic.dropped_p0_traces).unwrap_or(u64::MAX);
        result.pending_p0_spans =
            u64::try_from(headers.generic.dropped_p0_spans).unwrap_or(u64::MAX);
        let local_limit = if local_limit == 0 {
            DEFAULT_MAX_PAYLOAD_BYTES
        } else {
            local_limit
        };
        let limit = || {
            self.info_response_observer
                .max_request_bytes()
                // The Agent's limited reader needs one byte left to read EOF.
                .map_or(local_limit, |agent| local_limit.min(agent - 1))
        };
        let mut format = self.effective_output_format();
        let mut endpoint = Endpoint {
            url: format.add_path(&self.endpoint.url),
            ..self.endpoint.clone()
        };
        let stop = || cancel.is_cancelled() || Instant::now() >= deadline;
        let stopped = || {
            TraceExporterError::Io(io::Error::from(if cancel.is_cancelled() {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::TimedOut
            }))
        };
        let mut responses = ResponseState {
            interrupted,
            latest_feedback: None,
            diagnostic_bytes_remaining: MAX_DIAGNOSTIC_BYTES,
        };
        let mut planner = Payloads::new(&traces, &self.metadata, format, limit());
        planner.initial_capacity = self.serializer.batch_hint();
        loop {
            planner.set_limit(limit());
            let Some(payload) = planner.next(&stop) else {
                break;
            };
            let (counts, bytes) = match payload {
                Payload::Stopped => return Err(stopped()),
                Payload::Encoded { indices, bytes } => (
                    PayloadCounts {
                        chunks: indices.len(),
                        spans: traces[indices].iter().map(Vec::len).sum(),
                    },
                    bytes,
                ),
                Payload::Oversized { index } => {
                    result.oversized_chunks += 1;
                    result.oversized_spans += traces[index].len();
                    result.unattempted_chunks -= 1;
                    result.unattempted_spans -= traces[index].len();
                    continue;
                }
                Payload::Invalid { index, error } => {
                    let counts = PayloadCounts {
                        chunks: 1,
                        spans: traces[index].len(),
                    };
                    result.unattempted_chunks -= counts.chunks;
                    result.unattempted_spans -= counts.spans;
                    result.payloads.push(PayloadResult {
                        chunks: counts.chunks,
                        spans: counts.spans,
                        response: Err(error),
                        observations: observations(&Err(SendWithRetryError::Build(0)), 0, counts),
                    });
                    continue;
                }
            };
            if stop() {
                return Err(stopped());
            }
            self.serializer.remember_batch_size(bytes.len());
            let request_headers =
                self.serializer
                    .build_traces_headers(headers.clone(), counts.chunks, None);
            let completed_attempt = self
                .send_batch_payload(
                    &endpoint,
                    bytes,
                    request_headers,
                    counts,
                    result,
                    &mut responses,
                )
                .await;
            let rejected_v1 = self.disable_rejected_v1(format, result);
            if completed_attempt? {
                headers.generic.dropped_p0_traces = 0;
                headers.generic.dropped_p0_spans = 0;
            }
            if rejected_v1 {
                format = TraceExporterOutputFormat::V04;
                endpoint.url = format.add_path(&self.endpoint.url);
                planner.fallback_to_v04();
            }
        }
        if result.pending_p0_traces != 0 || result.pending_p0_spans != 0 {
            if stop() {
                return Err(stopped());
            }
            let bytes = super::payloads::empty(&self.metadata, format, limit(), &stop)?;
            let counts = PayloadCounts {
                chunks: 0,
                spans: 0,
            };
            let request_headers = self.serializer.build_traces_headers(headers, 0, None);
            let attempt = self
                .send_batch_payload(
                    &endpoint,
                    bytes,
                    request_headers,
                    counts,
                    result,
                    &mut responses,
                )
                .await;
            self.disable_rejected_v1(format, result);
            attempt?;
        }
        Ok(())
    }

    fn disable_rejected_v1(&self, format: TraceExporterOutputFormat, result: &BatchResult) -> bool {
        if format != TraceExporterOutputFormat::V1 || !result.payloads.last().is_some_and(|payload| {
            matches!(&payload.response, Err(TraceExporterError::Request(error)) if error.status() == 404)
        }) {
            return false;
        }
        if self
            .v1_active
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.info_response_observer.manual_trigger();
        }
        true
    }

    async fn send_batch_payload(
        &self,
        endpoint: &Endpoint,
        bytes: Vec<u8>,
        headers: http::HeaderMap,
        counts: PayloadCounts,
        batch: &mut BatchResult,
        responses: &mut ResponseState<impl Future<Output = io::ErrorKind> + Unpin>,
    ) -> Result<bool, TraceExporterError> {
        let size = bytes.len();
        let mut policy = BoundedResponse::new(MAX_FEEDBACK_BYTES);
        let outcome = tokio::select! {
            biased;
            kind = &mut responses.interrupted => Err(kind),
            result = async {
                batch.unattempted_chunks -= counts.chunks;
                batch.unattempted_spans -= counts.spans;
                batch.in_flight_chunks = counts.chunks;
                batch.in_flight_spans = counts.spans;
                send_with_bounded_response(&self.capabilities, endpoint, bytes, &headers,
                    &RetryStrategy::default(), CompressionStrategy::None, &mut policy).await
            } => Ok(result),
        };
        let (result, interrupted) = match outcome {
            Ok(result) => (result, None),
            Err(kind) => (
                policy
                    .observed_result()
                    .ok_or_else(|| io::Error::from(kind))?,
                Some(kind),
            ),
        };
        let completed_attempt = !matches!(&result, Err(SendWithRetryError::Build(_)));
        let report = observations(&result, size, counts);
        let response = match result {
            Ok((response, attempts)) => {
                self.info_response_observer.check_response(&response);
                self.emit_send_result(&SendResult::success(size, counts.chunks, attempts));
                if let Some(body) = sampling_feedback(response.body()) {
                    if let Some(previous) = responses.latest_feedback.take() {
                        batch.payloads[previous].response = Ok(AgentResponse::Unchanged);
                    }
                    responses.latest_feedback = Some(batch.payloads.len());
                    Ok(AgentResponse::Changed { body })
                } else {
                    Ok(AgentResponse::Unchanged)
                }
            }
            Err(SendWithRetryError::Http(mut response, attempts)) => {
                if response.status() == http::StatusCode::PAYLOAD_TOO_LARGE {
                    self.info_response_observer.manual_trigger();
                }
                let length = response
                    .body()
                    .len()
                    .min(responses.diagnostic_bytes_remaining);
                let text = String::from_utf8_lossy(&response.body()[..length]);
                let mut end = text.len().min(responses.diagnostic_bytes_remaining);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let diagnostic = Bytes::copy_from_slice(&text.as_bytes()[..end]);
                responses.diagnostic_bytes_remaining -= diagnostic.len();
                *response.body_mut() = diagnostic;
                self.handle_send_error(
                    SendWithRetryError::Http(response, attempts),
                    size,
                    counts.chunks,
                )
                .await
            }
            Err(error) => self.handle_send_error(error, size, counts.chunks).await,
        };
        batch.in_flight_chunks = 0;
        batch.in_flight_spans = 0;
        batch.payloads.push(PayloadResult {
            chunks: counts.chunks,
            spans: counts.spans,
            response,
            observations: report,
        });
        if completed_attempt {
            batch.pending_p0_traces = 0;
            batch.pending_p0_spans = 0;
        }
        if let Some(kind) = interrupted {
            return Err(io::Error::from(kind).into());
        }
        Ok(completed_attempt)
    }
}

#[cfg(test)]
mod tests;
