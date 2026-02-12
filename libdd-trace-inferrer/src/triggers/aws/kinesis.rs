// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS Kinesis trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger};
use crate::utils::{S_TO_NS, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct KinesisRecord {
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
    #[serde(rename = "eventVersion")]
    pub event_version: String,
    pub kinesis: KinesisEntity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct KinesisEntity {
    #[serde(rename = "approximateArrivalTimestamp")]
    pub approximate_arrival_timestamp: f64,
    #[serde(rename = "partitionKey")]
    pub partition_key: String,
    pub data: String,
}

impl Trigger for KinesisRecord {
    fn new(payload: Value) -> Option<Self> {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|first| {
                serde_json::from_value::<KinesisRecord>(first.clone())
                    .map_err(|e| debug!("Failed to deserialize Kinesis Record: {e}"))
                    .ok()
            })
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("kinesis"))
            .is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let stream_name = self.get_specific_service_id();
        let shard_id = self.event_id.split(':').next().unwrap_or_default();
        let service_name = resolve_service_name(
            &config.service_mapping,
            &stream_name,
            self.get_generic_service_id(),
            &stream_name,
            "kinesis",
            config.use_instance_service_names,
        );

        span.name = "aws.kinesis".to_string();
        span.service = service_name;
        span.start = (self.kinesis.approximate_arrival_timestamp * S_TO_NS) as i64;
        span.resource.clone_from(&stream_name);
        span.r#type = "web".to_string();
        span.meta = HashMap::from([
            ("operation_name".to_string(), "aws.kinesis".to_string()),
            ("stream_name".to_string(), stream_name),
            ("shard_id".to_string(), shard_id.to_string()),
            ("event_source_arn".to_string(), self.event_source_arn.clone()),
            ("event_id".to_string(), self.event_id.clone()),
            ("event_name".to_string(), self.event_name.clone()),
            ("event_version".to_string(), self.event_version.clone()),
            (
                "partition_key".to_string(),
                self.kinesis.partition_key.clone(),
            ),
        ]);
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "kinesis".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.event_source_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        if let Ok(decoded) = STANDARD.decode(&self.kinesis.data) {
            if let Ok(value) = serde_json::from_slice::<Value>(&decoded) {
                if let Some(carrier) = value.get(DATADOG_CARRIER_KEY) {
                    return serde_json::from_value(carrier.clone()).unwrap_or_default();
                }
            }
        }
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_specific_service_id(&self) -> String {
        self.event_source_arn
            .split('/')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_kinesis"
    }
}
