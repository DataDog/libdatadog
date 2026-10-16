// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trigger-specific inference logic.
//!
//! Each sub-module handles a specific trigger payload type.

pub mod aws;
mod serde_utils;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::HashMap;

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::span_link::SpanLink;

pub const DATADOG_CARRIER_KEY: &str = "_datadog";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_TAG: &str = "function_trigger.event_source";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG: &str = "function_trigger.event_source_arn";

/// Pre-extracted trace context from within the event payload.
///
/// Some triggers carry deterministic trace context embedded in the event
/// (e.g., AWSTraceHeader in SQS, SHA-256 context in Step Functions).
/// Consumers check this when carrier headers are empty.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceContext {
    pub trace_id: u64,
    pub span_id: u64,
    pub sampling_priority: Option<i8>,
    pub origin: Option<String>,
    pub tags: HashMap<String, String>,
}

/// Trait implemented by every trigger type.
///
/// Each trigger knows how to:
/// - Detect whether a payload matches it ([`Trigger::is_match`])
/// - Parse itself from a JSON value ([`Trigger::new`])
/// - Enrich a [`SpanData`] with extracted information
/// - Provide trigger tags, carrier, async flag, etc.
pub trait Trigger: Sized {
    /// Attempts to parse the payload into this trigger type.
    ///
    /// For record-based events (SQS, SNS, etc.) this extracts `Records[0]`.
    fn new(payload: Value) -> Option<Self>;

    /// Returns `true` if the payload matches this trigger type.
    ///
    /// This is a lightweight check that avoids full deserialization.
    fn is_match(payload: &Value) -> bool;

    /// Enriches a [`SpanData`] with data extracted from this trigger.
    ///
    /// If this method leaves `span.name` empty, no inferred span will be
    /// created. The trigger should handle service name resolution internally.
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig);

    /// Returns tags to attach to the invocation/function span.
    ///
    /// Must include `function_trigger.event_source`. Should also include
    /// `function_trigger.event_source_arn` and any other trigger-level tags
    /// (e.g., `dd_resource_key` for API Gateway).
    fn get_tags(&self, config: &InferConfig) -> HashMap<String, String>;

    /// Returns carrier headers for trace context extraction.
    ///
    /// The consumer passes these to their propagator to extract `SpanContext`.
    fn get_carrier(&self) -> HashMap<String, String>;

    /// Returns `true` if this trigger represents an asynchronous invocation.
    ///
    /// Async spans have different duration semantics: their duration is
    /// measured from event timestamp to invocation start, not to invocation
    /// end.
    fn is_async(&self) -> bool;

    /// Returns span links associated with this trigger event.
    ///
    /// Span links connect the inferred span to upstream resources (e.g., an
    /// S3 object or a DynamoDB item). Most triggers return an empty vec.
    fn get_span_links(&self) -> Vec<SpanLink> {
        Vec::new()
    }

    /// Returns a pre-extracted trace context from the payload itself.
    ///
    /// Some triggers carry deterministic trace context embedded in the event.
    /// Consumers check this when `get_carrier()` returns an empty map.
    fn get_trace_context(&self) -> Option<TraceContext> {
        None
    }
}

/// A macro to define an enum for all known trigger types.
///
/// Generates an enum with one variant per named type. Also creates
/// `from_value` and `from_slice` methods that try each trigger's
/// `is_match` + `new` in declaration order.
macro_rules! identified_triggers {
    (
        $vis:vis enum $name:ident {
            $($type:ty => $case:ident),+,
            else => $default:ident,
        }
    ) => {
        #[derive(Debug, Clone, Default)]
        #[must_use]
        #[non_exhaustive]
        $vis enum $name {
            $($case($type),)+
            #[default]
            $default,
        }

        impl $name {
            $vis fn from_value(payload: &Value) -> Self {
                $(
                if <$type>::is_match(payload) {
                    return <$type>::new(payload.clone()).map_or(Self::$default, Self::$case);
                }
                )+
                Self::$default
            }

            $vis fn from_slice(payload: &[u8]) -> serde_json::Result<Self> {
                let value: Value = serde_json::from_slice(payload)?;
                Ok(Self::from_value(&value))
            }

            #[must_use]
            $vis const fn is_unknown(&self) -> bool {
                matches!(self, Self::$default)
            }

            /// Builds an [`InferenceResult`](crate::inferrer::InferenceResult)
            /// by dispatching to the matched trigger's trait methods.
            $vis fn build_inference_result(
                &self,
                config: &$crate::config::InferConfig,
            ) -> $crate::inferrer::InferenceResult {
                match self {
                    $(Self::$case(t) => {
                        $crate::inferrer::build_result_from_trigger(t, config)
                    },)+
                    Self::$default => $crate::inferrer::InferenceResult::default(),
                }
            }
        }
    };
}

// The trigger type enum. Detection order matters: more specific types first.
identified_triggers!(
    pub enum TriggerType {
        aws::api_gateway_http::ApiGatewayHttpEvent => ApiGatewayHttp,
        aws::api_gateway_rest::ApiGatewayRestEvent => ApiGatewayRest,
        aws::api_gateway_websocket::ApiGatewayWebSocketEvent => ApiGatewayWebSocket,
        aws::alb::AlbEvent => Alb,
        aws::lambda_function_url::LambdaFunctionUrlEvent => LambdaFunctionUrl,
        aws::msk::MskEvent => Msk,
        aws::sqs::SqsRecord => Sqs,
        aws::sns::SnsRecord => Sns,
        aws::dynamodb::DynamoDbRecord => DynamoDb,
        aws::s3::S3Record => S3,
        aws::event_bridge::EventBridgeEvent => EventBridge,
        aws::kinesis::KinesisRecord => Kinesis,
        aws::step_function::StepFunctionEvent => StepFunction,
        else => Unknown,
    }
);

/// Deserialize a `HashMap` with lowercased keys.
///
/// HTTP headers are case-insensitive; this ensures consistent lookup.
pub fn lowercase_key<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let map = HashMap::<String, V>::deserialize(deserializer)?;
    Ok(map
        .into_iter()
        .map(|(key, value)| (key.to_lowercase(), value))
        .collect())
}
