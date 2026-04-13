// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS DynamoDB Streams trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::span_link::{SpanLink, generate_span_link_hash};
use crate::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger,
};
use crate::utils::{S_TO_NS, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbRecord {
    #[serde(rename = "dynamodb")]
    pub dynamodb: DynamoDbEntity,
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    #[serde(rename = "eventVersion")]
    pub event_version: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbEntity {
    #[serde(rename = "ApproximateCreationDateTime")]
    pub approximate_creation_date_time: f64,
    #[serde(rename = "SizeBytes")]
    pub size_bytes: i64,
    #[serde(rename = "StreamViewType")]
    pub stream_view_type: String,
    #[serde(rename = "Keys")]
    pub keys: HashMap<String, AttributeValue>,
}

/// DynamoDB attribute value. Can be String, Number (as string), or Binary
/// (base64-encoded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttributeValue {
    S(String),
    N(String),
    B(String),
}

impl AttributeValue {
    fn to_string_value(&self) -> Option<String> {
        match self {
            AttributeValue::S(s) => Some(s.clone()),
            AttributeValue::N(n) => Some(n.clone()),
            AttributeValue::B(b) => {
                use base64::{Engine, engine::general_purpose::STANDARD};
                STANDARD
                    .decode(b)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            }
        }
    }
}

impl DynamoDbRecord {
    fn get_specific_service_id(&self) -> String {
        self.event_source_arn
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_dynamodb"
    }
}

impl Trigger for DynamoDbRecord {
    fn new(payload: Value) -> Option<Self> {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|first| {
                serde_json::from_value::<DynamoDbRecord>(first.clone())
                    .map_err(|e| debug!("Failed to deserialize DynamoDB Record: {e}"))
                    .ok()
            })
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("dynamodb"))
            .is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let table_name = self.get_specific_service_id();
        let resource = format!("{} {}", self.event_name, table_name);
        let start_time = (self.dynamodb.approximate_creation_date_time * S_TO_NS) as i64;

        let service_name = resolve_service_name(
            &config.service_mapping,
            &table_name,
            self.get_generic_service_id(),
            &table_name,
            "dynamodb",
            config.use_instance_service_names,
        );

        span.name = "aws.dynamodb".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.dynamodb".to_string()),
            ("event_id".to_string(), self.event_id.clone()),
            ("event_name".to_string(), self.event_name.clone()),
            ("event_version".to_string(), self.event_version.clone()),
            ("event_source_arn".to_string(), self.event_source_arn.clone()),
            (
                "size_bytes".to_string(),
                self.dynamodb.size_bytes.to_string(),
            ),
            (
                "stream_view_type".to_string(),
                self.dynamodb.stream_view_type.clone(),
            ),
            ("table_name".to_string(), table_name),
        ]);
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        let mut tags = HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "dynamodb".to_string(),
        )]);

        // ARN tag
        tags.insert(
            FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
            self.event_source_arn.clone(),
        );

        tags
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_span_links(&self) -> Vec<SpanLink> {
        if self.dynamodb.keys.is_empty() {
            return Vec::new();
        }

        let table_name = self.get_specific_service_id();

        #[allow(clippy::single_match_else)]
        let result = match self.dynamodb.keys.len() {
            1 => {
                let Some((key, attr)) = self.dynamodb.keys.iter().next() else {
                    return Vec::new();
                };
                let Some(value) = attr.to_string_value() else {
                    return Vec::new();
                };
                (key.clone(), value, String::new(), String::new())
            }
            _ => {
                let mut keys: Vec<(&String, &AttributeValue)> = self.dynamodb.keys.iter().collect();
                keys.sort_by(|a, b| a.0.cmp(b.0));
                let Some(v1) = keys[0].1.to_string_value() else {
                    return Vec::new();
                };
                let Some(v2) = keys[1].1.to_string_value() else {
                    return Vec::new();
                };
                (keys[0].0.clone(), v1, keys[1].0.clone(), v2)
            }
        };

        let (pk1, v1, pk2, v2) = result;

        let parts = [
            table_name.as_str(),
            pk1.as_str(),
            v1.as_str(),
            pk2.as_str(),
            v2.as_str(),
        ];
        let hash = generate_span_link_hash(&parts);
        vec![SpanLink {
            hash,
            kind: "aws.dynamodb.item".to_string(),
        }]
    }
}
