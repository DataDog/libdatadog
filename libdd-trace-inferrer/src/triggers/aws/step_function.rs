// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS Step Functions trigger.
//!
//! Step Functions events are special: they generate a deterministic span
//! context rather than producing a normal inferred span.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    TraceContext, Trigger, DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
    FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tracing::debug;

pub const DATADOG_LEGACY_LAMBDA_PAYLOAD: &str = "Payload";

/// Higher-order trace ID bits key in tags.
const DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY: &str = "_dd.p.tid";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StepFunctionEvent {
    #[serde(rename = "Execution")]
    pub execution: Execution,
    #[serde(rename = "State")]
    pub state: State,
    #[serde(rename = "StateMachine")]
    pub state_machine: Option<StateMachine>,
    #[serde(rename = "x-datadog-trace-id")]
    pub trace_id: Option<String>,
    #[serde(rename = "x-datadog-tags")]
    pub trace_tags: Option<String>,
    #[serde(rename = "RootExecutionId")]
    pub root_execution_id: Option<String>,
    #[serde(rename = "serverless-version")]
    pub serverless_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Execution {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "RedriveCount")]
    pub redrive_count: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct State {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "EnteredTime")]
    pub entered_time: String,
    #[serde(rename = "RetryCount")]
    pub retry_count: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StateMachine {
    #[serde(rename = "Id")]
    pub id: String,
}

impl Trigger for StepFunctionEvent {
    fn new(payload: Value) -> Option<Self> {
        let p = payload
            .get(DATADOG_LEGACY_LAMBDA_PAYLOAD)
            .unwrap_or(&payload)
            .get(DATADOG_CARRIER_KEY)
            .unwrap_or(
                payload
                    .get(DATADOG_LEGACY_LAMBDA_PAYLOAD)
                    .unwrap_or(&payload),
            );

        match serde_json::from_value::<StepFunctionEvent>(p.clone()) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize Step Function Event: {e}");
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        let p = payload
            .get(DATADOG_LEGACY_LAMBDA_PAYLOAD)
            .unwrap_or(payload)
            .get(DATADOG_CARRIER_KEY)
            .unwrap_or(
                payload
                    .get(DATADOG_LEGACY_LAMBDA_PAYLOAD)
                    .unwrap_or(payload),
            );

        let execution_id = p
            .get("Execution")
            .and_then(Value::as_object)
            .and_then(|e| e.get("Id"));
        let state = p.get("State").and_then(Value::as_object);
        let name = state.and_then(|s| s.get("Name"));
        let entered_time = state.and_then(|s| s.get("EnteredTime"));

        execution_id.is_some() && name.is_some() && entered_time.is_some()
    }

    fn enrich_span(&self, _span: &mut SpanData, _config: &InferConfig) {
        // Step Functions events do not produce normal inferred spans.
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        let mut tags = HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "states".to_string(),
        )]);

        // ARN tag
        let arn = self
            .state_machine
            .as_ref()
            .map_or_else(String::new, |sm| sm.id.clone());
        tags.insert(FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(), arn);

        tags
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_trace_context(&self) -> Option<TraceContext> {
        Some(self.build_span_context())
    }
}

impl StepFunctionEvent {
    fn build_span_context(&self) -> TraceContext {
        let (lo_tid, tags) =
            if let (Some(trace_id), Some(trace_tags)) = (&self.trace_id, &self.trace_tags) {
                // Lambda Root
                let lo_tid = trace_id
                    .parse()
                    .unwrap_or(Self::generate_trace_id(&self.execution.id).0);

                let tags = Self::extract_tags(trace_tags);
                (lo_tid, tags)
            } else {
                let execution_arn = self
                    .root_execution_id
                    .as_ref()
                    .unwrap_or(&self.execution.id);
                let (lo_tid, hi_tid) = Self::generate_trace_id(execution_arn);
                let tags = HashMap::from([(
                    DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
                    format!("{hi_tid:x}"),
                )]);
                (lo_tid, tags)
            };

        let parent_id = Self::generate_parent_id(
            &self.execution.id,
            &self.state.name,
            &self.state.entered_time,
            self.state.retry_count,
            self.execution.redrive_count,
        );

        TraceContext {
            trace_id: lo_tid,
            span_id: parent_id,
            sampling_priority: Some(1),
            origin: Some("states".to_string()),
            tags,
        }
    }

    fn generate_parent_id(
        execution_id: &str,
        state_name: &str,
        state_entered_time: &str,
        retry_count: u16,
        redrive_count: u16,
    ) -> u64 {
        let mut unique_string = format!("{execution_id}#{state_name}#{state_entered_time}");

        if retry_count != 0 || redrive_count != 0 {
            unique_string.push_str(&format!("#{retry_count}#{redrive_count}"));
        }

        let hash = Sha256::digest(unique_string.as_bytes());
        Self::get_positive_u64(&hash[0..8])
    }

    fn generate_trace_id(execution_arn: &str) -> (u64, u64) {
        let hash = Sha256::digest(execution_arn.as_bytes());
        let lower_order_bits = Self::get_positive_u64(&hash[8..16]);
        let higher_order_bits = Self::get_positive_u64(&hash[0..8]);
        (lower_order_bits, higher_order_bits)
    }

    fn get_positive_u64(hash_bytes: &[u8]) -> u64 {
        let mut result: u64 = hash_bytes
            .iter()
            .take(8)
            .fold(0, |acc, &byte| (acc << 8) + u64::from(byte));
        result &= !(1u64 << 63);
        if result == 0 {
            1
        } else {
            result
        }
    }

    /// Extracts DD tags from the `x-datadog-tags` header value.
    fn extract_tags(tags_str: &str) -> HashMap<String, String> {
        let mut tags = HashMap::new();
        for pair in tags_str.split(',') {
            if let Some((key, value)) = pair.split_once('=') {
                tags.insert(key.to_string(), value.to_string());
            }
        }
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_parent_id() {
        let parent_id = StepFunctionEvent::generate_parent_id(
            "arn:aws:states:sa-east-1:601427271234:express:DatadogStateMachine:acaf1a67-336a-e854-1599-2a627eb2dd8a:c8baf081-31f1-464d-971f-70cb17d01111",
            "step-one",
            "2022-12-08T21:08:19.224Z",
            0,
            0,
        );
        assert_eq!(parent_id, 4_340_734_536_022_949_921);
    }

    #[test]
    fn test_generate_trace_id() {
        let (lo_tid, hi_tid) = StepFunctionEvent::generate_trace_id(
            "arn:aws:states:us-east-1:425362996713:execution:agocsTestSF:bc9f281c-3daa-4e5a-9a60-471a3810bf44",
        );
        assert_eq!(lo_tid, 5_744_042_798_732_701_615);
        assert_eq!(hi_tid, 1_807_349_139_850_867_390);
    }

    #[test]
    fn test_is_match() {
        let payload = serde_json::json!({
            "Execution": {
                "Id": "arn:aws:states:us-east-1:123:execution:test:abc",
                "RedriveCount": 0
            },
            "State": {
                "Name": "step1",
                "EnteredTime": "2024-01-01T00:00:00Z",
                "RetryCount": 0
            },
            "StateMachine": {
                "Id": "arn:aws:states:us-east-1:123:stateMachine:test"
            }
        });
        assert!(StepFunctionEvent::is_match(&payload));
    }
}
