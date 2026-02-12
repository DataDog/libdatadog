// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Main span inferrer and [`InferenceResult`] type.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::InferConfig;
use crate::error::InferrerError;
use crate::span_data::SpanData;
use crate::span_pointer::SpanPointer;
use crate::triggers::{
    GeneratedTraceContext, Trigger, TriggerType, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
};
use crate::triggers::aws::sqs::WrappedSqsTrigger;

/// Complete result of inferring trace data from a payload.
///
/// Consumers use the fields of this struct to:
/// - Construct a trace span from [`span_data`](Self::span_data)
/// - Add trigger tags to the invocation/function span
/// - Extract trace context from [`carrier`](Self::carrier) or
///   [`generated_context`](Self::generated_context)
/// - Determine duration semantics from [`is_async`](Self::is_async)
/// - Optionally create a wrapped inferred span (e.g., SNS-in-SQS)
#[derive(Debug, Clone, Default)]
pub struct InferenceResult {
    // ── Span data ───────────────────────────────────────────────────────
    /// Data for the inferred span (decoupled from any protobuf type).
    pub span_data: SpanData,

    // ── Trigger metadata ────────────────────────────────────────────────
    /// Tags to add to the invocation/function span (NOT the inferred span).
    pub trigger_tags: HashMap<String, String>,
    /// The detected trigger type (the enum variant wraps the parsed trigger).
    pub trigger_type: TriggerType,

    // ── Trace context propagation ───────────────────────────────────────
    /// Carrier headers for trace context extraction (e.g., Datadog headers).
    ///
    /// Consumers pass these to their propagator's `extract()` method.
    pub carrier: HashMap<String, String>,
    /// Deterministically generated trace context (Step Functions, AWSTraceHeader).
    ///
    /// Consumers should check this when `carrier` is empty.
    pub generated_context: Option<GeneratedTraceContext>,

    // ── Behavioral flags ────────────────────────────────────────────────
    /// Whether this trigger is an asynchronous invocation.
    ///
    /// For async triggers, span duration = invocation_start - event_time.
    /// For sync triggers, span duration = invocation_end - event_time.
    pub is_async: bool,
    /// Whether to actually create an inferred span.
    ///
    /// `false` for unknown payloads, ALB events, and Step Functions with
    /// generated context. When `false`, consumers should still use
    /// `trigger_tags` and `carrier`.
    pub should_create_inferred_span: bool,

    // ── AWS-specific data ───────────────────────────────────────────────
    /// ARN of the trigger event source.
    pub event_source_arn: String,
    /// `dd_resource_key` for API Gateway cloud integrations linking.
    pub dd_resource_key: Option<String>,
    /// Span pointers for S3 and DynamoDB stream events.
    pub span_pointers: Vec<SpanPointer>,

    // ── Wrapped inferred span ───────────────────────────────────────────
    /// Optional nested inferred span (e.g., SNS event inside SQS body,
    /// EventBridge event inside SQS/SNS body).
    pub wrapped_span: Option<Box<InferenceResult>>,
}

/// Builds an [`InferenceResult`] from a trigger implementing the [`Trigger`]
/// trait.
///
/// Called by the macro-generated `TriggerType::build_inference_result` method.
pub fn build_result_from_trigger<T: Trigger>(trigger: &T, config: &InferConfig) -> InferenceResult {
    let mut span_data = SpanData::default();
    trigger.enrich_span(&mut span_data, config);

    let mut trigger_tags = trigger.get_tags();
    let carrier = trigger.get_carrier();
    let is_async = trigger.is_async();

    let arn = trigger.get_arn(&config.region);
    let dd_resource_key = trigger.get_dd_resource_key(&config.region);
    let span_pointers = trigger.get_span_pointers().unwrap_or_default();
    let generated_context = trigger.get_generated_trace_context();

    // Only create an inferred span if enrich_span populated the span name.
    // ALB and Step Functions leave enrich_span as a no-op, so name stays empty.
    let should_create = !span_data.name.is_empty();

    if !arn.is_empty() {
        trigger_tags.insert(
            FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
            arn.clone(),
        );
    }

    InferenceResult {
        span_data,
        trigger_tags,
        trigger_type: TriggerType::Unknown, // Set by macro caller
        carrier,
        generated_context,
        is_async,
        should_create_inferred_span: should_create,
        event_source_arn: arn,
        dd_resource_key,
        span_pointers,
        wrapped_span: None,
    }
}

/// The main span inferrer.
///
/// Holds configuration and provides methods to infer spans from JSON
/// payloads.
pub struct SpanInferrer {
    config: InferConfig,
}

impl SpanInferrer {
    /// Creates a new span inferrer with the given configuration.
    #[must_use]
    pub fn new(config: InferConfig) -> Self {
        Self { config }
    }

    /// Infers trace data from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`InferrerError::InvalidJson`] if the payload is not valid JSON.
    pub fn infer_span(&self, payload: &str) -> Result<InferenceResult, InferrerError> {
        let value: Value = serde_json::from_str(payload)?;
        Ok(self.infer_span_from_value(&value))
    }

    /// Infers trace data from a JSON byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`InferrerError::InvalidJson`] if the payload is not valid JSON.
    pub fn infer_span_from_bytes(&self, payload: &[u8]) -> Result<InferenceResult, InferrerError> {
        let value: Value = serde_json::from_slice(payload)?;
        Ok(self.infer_span_from_value(&value))
    }

    /// Infers trace data from a pre-parsed JSON value.
    ///
    /// This never fails; unknown payloads return a result with
    /// `should_create_inferred_span: false`.
    #[must_use]
    pub fn infer_span_from_value(&self, payload: &Value) -> InferenceResult {
        let identified = TriggerType::from_value(payload);

        if identified.is_unknown() {
            return InferenceResult::default();
        }

        let mut result = identified.build_inference_result(&self.config);

        // Build wrapped inferred span for SQS and SNS triggers.
        result.wrapped_span = self.build_wrapped_span(&result);

        result
    }

    /// Builds a wrapped inferred span for triggers that contain nested events.
    ///
    /// - SQS can wrap SNS or EventBridge events in its body.
    /// - SNS can wrap EventBridge events in its message.
    fn build_wrapped_span(&self, result: &InferenceResult) -> Option<Box<InferenceResult>> {
        match &result.trigger_type {
            TriggerType::Sqs(sqs_record) => {
                let wrapped = sqs_record.get_wrapped_trigger()?;
                let mut wrapped_span = SpanData::default();

                let wrapped_tags = match &wrapped {
                    WrappedSqsTrigger::Sns(sns_record) => {
                        sns_record.enrich_span(&mut wrapped_span, &self.config);
                        sns_record.get_tags()
                    }
                    WrappedSqsTrigger::EventBridge(eb_event) => {
                        eb_event.enrich_span(&mut wrapped_span, &self.config);
                        eb_event.get_tags()
                    }
                };

                Some(Box::new(InferenceResult {
                    span_data: wrapped_span,
                    trigger_tags: wrapped_tags,
                    should_create_inferred_span: true,
                    ..InferenceResult::default()
                }))
            }
            TriggerType::Sns(sns_record) => {
                let eb_event = sns_record.get_wrapped_trigger()?;
                let mut wrapped_span = SpanData::default();
                eb_event.enrich_span(&mut wrapped_span, &self.config);
                let wrapped_tags = eb_event.get_tags();

                Some(Box::new(InferenceResult {
                    span_data: wrapped_span,
                    trigger_tags: wrapped_tags,
                    should_create_inferred_span: true,
                    ..InferenceResult::default()
                }))
            }
            _ => None,
        }
    }
}

/// Context provided by the consumer for completing inferred spans.
///
/// Contains invocation-level data that is only known at invocation time,
/// not at payload parsing time.
#[derive(Debug, Clone)]
pub struct CompletionContext {
    /// Trace ID assigned by the consumer's tracer.
    pub trace_id: u64,
    /// Invocation start time in nanoseconds since Unix epoch.
    pub invocation_start_ns: i64,
    /// Invocation duration in nanoseconds.
    pub invocation_duration_ns: i64,
    /// Service name of the invocation/function span.
    pub invocation_service: String,
    /// Whether the invocation resulted in an error.
    pub is_error: bool,
}

/// A completed inferred span with all IDs, durations, and metadata set.
///
/// This is the final output ready for consumers to convert into their
/// native span representation.
#[derive(Debug, Clone, Default)]
pub struct CompletedSpan {
    /// The fully enriched span data.
    pub span_data: SpanData,
    /// Duration in nanoseconds.
    pub duration_ns: i64,
    /// Trace ID (set from invocation context).
    pub trace_id: u64,
    /// Parent span ID (set by consumer or chained from wrapped span).
    pub parent_id: u64,
    /// Whether the invocation had an error.
    pub is_error: bool,
}

/// Output of [`complete_inference`]: ready-to-use spans including any
/// wrapped span with correct parent chaining.
#[derive(Debug, Clone, Default)]
pub struct CompletedSpans {
    /// The primary inferred span, if one should be created.
    pub inferred_span: Option<CompletedSpan>,
    /// Optional wrapped inferred span (e.g., SNS inside SQS).
    ///
    /// When present, consumers should chain parent IDs:
    /// 1. Assign `span_id` values to both spans.
    /// 2. Set `inferred_span.parent_id = wrapped_span.span_id`.
    ///
    /// The `wrapped_span.parent_id` is already set to the original
    /// `parent_id` passed to [`complete_inference`].
    pub wrapped_span: Option<CompletedSpan>,
}

/// Completes an [`InferenceResult`] with invocation-time context.
///
/// This pure function handles the tricky duration calculation and
/// parent-ID chaining that varies between async/sync triggers and
/// wrapped spans. Consumers call this after inference and after
/// determining the parent span ID.
///
/// ## Duration semantics
///
/// - **Async triggers**: `duration = invocation_start - inferred_start`
/// - **Sync triggers**: `duration = (invocation_start + invocation_duration) - inferred_start`
///
/// For wrapped spans, `duration = inferred_start - wrapped_start`.
///
/// ## Parent chaining (when a wrapped span exists)
///
/// The returned `wrapped_span.parent_id` is set to the `parent_id`
/// argument (the original parent). Consumers must then set
/// `inferred_span.parent_id` to the `span_id` they assign to the
/// wrapped span. This library does not generate random span IDs.
#[must_use]
pub fn complete_inference(
    result: &InferenceResult,
    parent_id: u64,
    ctx: &CompletionContext,
) -> CompletedSpans {
    if !result.should_create_inferred_span {
        return CompletedSpans::default();
    }

    let mut span_data = result.span_data.clone();
    span_data.meta.insert(
        "peer.service".to_string(),
        ctx.invocation_service.clone(),
    );
    span_data
        .meta
        .insert("span.kind".to_string(), "server".to_string());

    let duration_ns = if result.is_async {
        ctx.invocation_start_ns - span_data.start
    } else {
        (ctx.invocation_start_ns + ctx.invocation_duration_ns) - span_data.start
    };

    let inferred = CompletedSpan {
        span_data,
        duration_ns,
        trace_id: ctx.trace_id,
        parent_id,
        is_error: ctx.is_error,
    };

    let wrapped = result.wrapped_span.as_ref().map(|ws| {
        let mut ws_span_data = ws.span_data.clone();
        ws_span_data.meta.insert(
            "peer.service".to_string(),
            inferred.span_data.service.clone(),
        );

        let ws_duration = inferred.span_data.start - ws_span_data.start;

        CompletedSpan {
            span_data: ws_span_data,
            duration_ns: ws_duration,
            trace_id: ctx.trace_id,
            parent_id: inferred.parent_id,
            is_error: ctx.is_error,
        }
    });

    CompletedSpans {
        inferred_span: Some(inferred),
        wrapped_span: wrapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_unknown_payload() {
        let inferrer = SpanInferrer::new(InferConfig::default());
        let result = inferrer.infer_span(r#"{"random": "data"}"#).unwrap();
        assert!(matches!(result.trigger_type, TriggerType::Unknown));
        assert!(!result.should_create_inferred_span);
    }

    #[test]
    fn test_infer_invalid_json() {
        let inferrer = SpanInferrer::new(InferConfig::default());
        let result = inferrer.infer_span("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_inference_result_default() {
        let result = InferenceResult::default();
        assert!(!result.should_create_inferred_span);
        assert!(result.carrier.is_empty());
        assert!(result.trigger_tags.is_empty());
        assert!(result.span_pointers.is_empty());
        assert!(result.wrapped_span.is_none());
        assert!(result.generated_context.is_none());
    }

    #[test]
    fn test_complete_inference_sync_span() {
        let result = InferenceResult {
            span_data: SpanData {
                name: "aws.sqs".to_string(),
                service: "sqs".to_string(),
                resource: "my-queue".to_string(),
                r#type: "web".to_string(),
                start: 1_000_000_000,
                ..Default::default()
            },
            should_create_inferred_span: true,
            is_async: false,
            ..Default::default()
        };

        let ctx = CompletionContext {
            trace_id: 12345,
            invocation_start_ns: 2_000_000_000,
            invocation_duration_ns: 500_000_000,
            invocation_service: "my-lambda".to_string(),
            is_error: false,
        };

        let completed = complete_inference(&result, 999, &ctx);
        let span = completed.inferred_span.unwrap();

        assert_eq!(span.trace_id, 12345);
        assert_eq!(span.parent_id, 999);
        assert!(!span.is_error);
        // Sync: (2s + 0.5s) - 1s = 1.5s
        assert_eq!(span.duration_ns, 1_500_000_000);
        assert_eq!(span.span_data.meta.get("peer.service").unwrap(), "my-lambda");
        assert_eq!(span.span_data.meta.get("span.kind").unwrap(), "server");
        assert!(completed.wrapped_span.is_none());
    }

    #[test]
    fn test_complete_inference_async_span() {
        let result = InferenceResult {
            span_data: SpanData {
                name: "aws.sqs".to_string(),
                service: "sqs".to_string(),
                start: 1_000_000_000,
                ..Default::default()
            },
            should_create_inferred_span: true,
            is_async: true,
            ..Default::default()
        };

        let ctx = CompletionContext {
            trace_id: 12345,
            invocation_start_ns: 3_000_000_000,
            invocation_duration_ns: 500_000_000,
            invocation_service: "my-lambda".to_string(),
            is_error: true,
        };

        let completed = complete_inference(&result, 999, &ctx);
        let span = completed.inferred_span.unwrap();

        assert!(span.is_error);
        // Async: 3s - 1s = 2s
        assert_eq!(span.duration_ns, 2_000_000_000);
    }

    #[test]
    fn test_complete_inference_with_wrapped_span() {
        let wrapped_result = InferenceResult {
            span_data: SpanData {
                name: "aws.sns".to_string(),
                service: "sns".to_string(),
                start: 500_000_000,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = InferenceResult {
            span_data: SpanData {
                name: "aws.sqs".to_string(),
                service: "sqs".to_string(),
                start: 1_000_000_000,
                ..Default::default()
            },
            should_create_inferred_span: true,
            is_async: true,
            wrapped_span: Some(Box::new(wrapped_result)),
            ..Default::default()
        };

        let ctx = CompletionContext {
            trace_id: 12345,
            invocation_start_ns: 3_000_000_000,
            invocation_duration_ns: 500_000_000,
            invocation_service: "my-lambda".to_string(),
            is_error: false,
        };

        let completed = complete_inference(&result, 999, &ctx);
        let inferred = completed.inferred_span.unwrap();
        let wrapped = completed.wrapped_span.unwrap();

        // Inferred span: async duration = 3s - 1s = 2s
        assert_eq!(inferred.duration_ns, 2_000_000_000);
        assert_eq!(inferred.parent_id, 999);

        // Wrapped span takes the original parent
        assert_eq!(wrapped.parent_id, 999);
        // Wrapped duration = inferred_start - wrapped_start = 1s - 0.5s
        assert_eq!(wrapped.duration_ns, 500_000_000);
        // Wrapped span's peer.service is the inferred span's service
        assert_eq!(wrapped.span_data.meta.get("peer.service").unwrap(), "sqs");
        assert_eq!(wrapped.trace_id, 12345);
    }

    #[test]
    fn test_complete_inference_no_span_created() {
        let result = InferenceResult {
            should_create_inferred_span: false,
            ..Default::default()
        };

        let ctx = CompletionContext {
            trace_id: 12345,
            invocation_start_ns: 1_000_000_000,
            invocation_duration_ns: 500_000_000,
            invocation_service: "my-lambda".to_string(),
            is_error: false,
        };

        let completed = complete_inference(&result, 999, &ctx);
        assert!(completed.inferred_span.is_none());
        assert!(completed.wrapped_span.is_none());
    }
}
