// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS MSK (Managed Streaming for Kafka) trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger,
};
use crate::utils::{MS_TO_NS, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MskEvent {
    pub event_source: String,
    pub event_source_arn: String,
    pub records: HashMap<String, Vec<MskRecord>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MskRecord {
    pub topic: String,
    pub partition: i32,
    pub timestamp: f64,
}

impl MskEvent {
    const GENERIC_SERVICE_KEY: &'static str = "lambda_msk";

    fn service_id(&self) -> String {
        self.event_source_arn
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }
}

impl Trigger for MskEvent {
    fn new(mut payload: Value) -> Option<Self> {
        // Only keep the first record of the first topic
        if let Some(records_map) = payload.get_mut("records").and_then(Value::as_object_mut) {
            match records_map.iter_mut().next() {
                Some((first_key, Value::Array(arr))) => {
                    arr.truncate(1);
                    let key = first_key.clone();
                    records_map.retain(|k, _| k == &key);
                }
                _ => {
                    records_map.clear();
                }
            }
        }

        match serde_json::from_value::<Self>(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize MSKEvent: {e}");
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("records")
            .and_then(Value::as_object)
            .and_then(|map| map.values().next())
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .is_some_and(|rec| rec.get("topic").is_some())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        span.name = "aws.msk".to_string();
        span.service = resolve_service_name(
            &config.service_mapping,
            &self.service_id(),
            Self::GENERIC_SERVICE_KEY,
            &self.service_id(),
            "msk",
            config.use_instance_service_names,
        );
        span.r#type = "web".to_string();

        let first_value = self.records.values().find_map(|arr| arr.first());
        if let Some(first_value) = first_value {
            span.resource.clone_from(&first_value.topic);
            span.start = (first_value.timestamp * MS_TO_NS) as i64;
            span.meta.extend([
                ("operation_name".to_string(), "aws.msk".to_string()),
                ("topic".to_string(), first_value.topic.clone()),
                ("partition".to_string(), first_value.partition.to_string()),
                ("event_source".to_string(), self.event_source.clone()),
                ("event_source_arn".to_string(), self.event_source_arn.clone()),
            ]);
        }
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        HashMap::from([
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "msk".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                self.event_source_arn.clone(),
            ),
        ])
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}
