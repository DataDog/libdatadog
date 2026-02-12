// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS SQS trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, GeneratedTraceContext, Trigger,
};
use crate::utils::{MS_TO_NS, get_aws_partition_by_region, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::event_bridge::EventBridgeEvent;
use super::sns::{SnsEntity, SnsRecord};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SqsRecord {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "receiptHandle")]
    pub receipt_handle: String,
    pub attributes: SqsAttributes,
    #[serde(rename = "messageAttributes")]
    pub message_attributes: HashMap<String, SqsMessageAttribute>,
    #[serde(rename = "md5OfBody")]
    pub md5_of_body: String,
    #[serde(rename = "eventSource")]
    pub event_source: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
    #[serde(rename = "awsRegion")]
    pub aws_region: String,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SqsAttributes {
    #[serde(rename = "ApproximateFirstReceiveTimestamp")]
    pub approximate_first_receive_timestamp: String,
    #[serde(rename = "ApproximateReceiveCount")]
    pub approximate_receive_count: String,
    #[serde(rename = "SentTimestamp")]
    pub sent_timestamp: String,
    #[serde(rename = "SenderId")]
    pub sender_id: String,
    #[serde(rename = "AWSTraceHeader")]
    pub aws_trace_header: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SqsMessageAttribute {
    #[serde(rename = "stringValue")]
    pub string_value: Option<String>,
    #[serde(rename = "binaryValue")]
    pub binary_value: Option<String>,
    #[serde(rename = "stringListValues")]
    pub string_list_values: Option<Vec<String>>,
    #[serde(rename = "binaryListValues")]
    pub binary_list_values: Option<Vec<String>>,
    #[serde(rename = "dataType")]
    pub data_type: String,
}

impl Trigger for SqsRecord {
    fn new(payload: Value) -> Option<Self> {
        let records = payload.get("Records").and_then(Value::as_array);
        match records {
            Some(records) => match serde_json::from_value::<SqsRecord>(records[0].clone()) {
                Ok(event) => Some(event),
                Err(e) => {
                    debug!("Failed to deserialize SQS Record: {e}");
                    None
                }
            },
            None => None,
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("eventSource"))
            .and_then(Value::as_str)
            .is_some_and(|s| s == "aws:sqs")
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let resource = self.get_specific_service_id();
        let start_time = (self
            .attributes
            .sent_timestamp
            .parse::<i64>()
            .unwrap_or_default() as f64
            * MS_TO_NS) as i64;

        let service_name = resolve_service_name(
            &config.service_mapping,
            &self.get_specific_service_id(),
            self.get_generic_service_id(),
            &self.get_specific_service_id(),
            "sqs",
            config.use_instance_service_names,
        );

        span.name = "aws.sqs".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.sqs".to_string()),
            ("receipt_handle".to_string(), self.receipt_handle.clone()),
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.attributes.sender_id.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
        ]);
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.attributes.sender_id.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "sqs".to_string(),
            ),
        ])
    }

    fn get_arn(&self, region: &str) -> String {
        if let [_, _, _, _, account, queue_name] = self
            .event_source_arn
            .split(':')
            .collect::<Vec<&str>>()
            .as_slice()
        {
            format!(
                "arn:{}:sqs:{}:{}:{}",
                get_aws_partition_by_region(region),
                region,
                account,
                queue_name
            )
        } else {
            String::new()
        }
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        // Check messageAttributes._datadog
        if let Some(ma) = self.message_attributes.get(DATADOG_CARRIER_KEY) {
            if let Some(string_value) = &ma.string_value {
                return serde_json::from_str(string_value).unwrap_or_default();
            }
        }

        // Fallback: check for SNS event in SQS body
        if let Ok(sns_entity) = serde_json::from_str::<SnsEntity>(&self.body) {
            let sns_record = SnsRecord {
                sns: sns_entity,
                event_subscription_arn: None,
            };
            return sns_record.get_carrier();
        }

        // Fallback: check for EventBridge event in SQS body
        if let Ok(event) = serde_json::from_str::<EventBridgeEvent>(&self.body) {
            return event.get_carrier();
        }

        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_specific_service_id(&self) -> String {
        self.event_source_arn
            .split(':')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_sqs"
    }

    fn get_generated_trace_context(&self) -> Option<GeneratedTraceContext> {
        extract_trace_context_from_aws_trace_header(self.attributes.aws_trace_header.clone())
    }
}

/// Extracts trace context from the `AWSTraceHeader` attribute.
///
/// Format: `Root=1-xxx-yyy;Parent=zzz;Sampled=1`
pub fn extract_trace_context_from_aws_trace_header(
    header: Option<String>,
) -> Option<GeneratedTraceContext> {
    let value = header?;
    if !value.starts_with("Root=") {
        return None;
    }

    let mut trace_id_hex = String::new();
    let mut parent_id_hex = String::new();
    let mut sampled = String::new();

    for part in value.split(';') {
        if part.starts_with("Root=") && part.len() > 24 {
            trace_id_hex = part[24..].to_string();
        } else if let Some(parent_part) = part.strip_prefix("Parent=") {
            parent_id_hex = parent_part.to_string();
        } else if part.starts_with("Sampled=") && sampled.is_empty() {
            sampled = part[8..].to_string();
        }
        if !trace_id_hex.is_empty() && !parent_id_hex.is_empty() && !sampled.is_empty() {
            break;
        }
    }

    let trace_id = u64::from_str_radix(&trace_id_hex, 16).ok()?;
    let parent_id = u64::from_str_radix(&parent_id_hex, 16).ok()?;

    if trace_id == 0 || parent_id == 0 {
        debug!("AWSTraceHeader contains empty trace or parent ID");
        return None;
    }

    let sampling_priority = if sampled == "1" { Some(1i8) } else { Some(0) };

    Some(GeneratedTraceContext {
        trace_id,
        span_id: parent_id,
        sampling_priority,
        origin: None,
        tags: HashMap::new(),
    })
}

/// Returns the wrapped inferred span data (SNS or EventBridge wrapped in SQS).
///
/// Used by the inferrer to create wrapped inferred spans.
impl SqsRecord {
    pub fn get_wrapped_trigger(&self) -> Option<WrappedSqsTrigger> {
        if let Ok(sns_entity) = serde_json::from_str::<SnsEntity>(&self.body) {
            return Some(WrappedSqsTrigger::Sns(SnsRecord {
                sns: sns_entity,
                event_subscription_arn: None,
            }));
        }
        if let Ok(event) = serde_json::from_str::<EventBridgeEvent>(&self.body) {
            return Some(WrappedSqsTrigger::EventBridge(event));
        }
        None
    }
}

/// A trigger type found wrapped inside an SQS message body.
pub enum WrappedSqsTrigger {
    Sns(SnsRecord),
    EventBridge(EventBridgeEvent),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_match() {
        let payload: Value =
            serde_json::from_str(r#"{"Records":[{"eventSource":"aws:sqs"}]}"#).unwrap();
        assert!(SqsRecord::is_match(&payload));
    }

    #[test]
    fn test_extract_aws_trace_header() {
        let header =
            "Root=1-68029e8a-0000000035578e774943fd9d;Parent=76c040bdc454a7ac;Sampled=1";
        let ctx =
            extract_trace_context_from_aws_trace_header(Some(header.to_string())).unwrap();
        assert_eq!(ctx.trace_id, 0x3557_8e77_4943_fd9d);
        assert_eq!(ctx.span_id, 0x76c0_40bd_c454_a7ac);
        assert_eq!(ctx.sampling_priority, Some(1));
    }
}
