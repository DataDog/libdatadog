// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS EventBridge trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
    Trigger,
};
use crate::utils::{MS_TO_NS, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

const DATADOG_START_TIME_KEY: &str = "x-datadog-start-time";
const DATADOG_RESOURCE_NAME_KEY: &str = "x-datadog-resource-name";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EventBridgeEvent {
    pub id: String,
    pub version: String,
    pub account: String,
    pub time: String,
    pub region: String,
    pub resources: Vec<String>,
    pub source: String,
    #[serde(rename = "detail-type")]
    pub detail_type: String,
    pub detail: Value,
    #[serde(rename = "replay-name")]
    pub replay_name: Option<String>,
}

impl EventBridgeEvent {
    const GENERIC_SERVICE_KEY: &'static str = "lambda_eventbridge";

    fn service_id_from_carrier(&self, carrier: &HashMap<String, String>) -> String {
        carrier
            .get(DATADOG_RESOURCE_NAME_KEY)
            .unwrap_or(&self.source)
            .to_string()
    }
}

impl Trigger for EventBridgeEvent {
    fn new(payload: Value) -> Option<Self> {
        match serde_json::from_value(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize EventBridge Event: {e}");
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload.get("detail-type").is_some()
            && payload
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "aws.events")
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        // Parse event time as fallback
        let start_time_fallback = chrono::DateTime::parse_from_rfc3339(&self.time)
            .map(|dt| {
                dt.timestamp_nanos_opt()
                    .unwrap_or((dt.timestamp_millis() as f64 * MS_TO_NS) as i64)
            })
            .unwrap_or(0);

        let carrier = self.get_carrier();
        let resource_name = self.service_id_from_carrier(&carrier);
        let start_time = carrier
            .get(DATADOG_START_TIME_KEY)
            .and_then(|s| s.parse::<f64>().ok())
            .map_or(start_time_fallback, |s| (s * MS_TO_NS) as i64);

        let service_name = resolve_service_name(
            &config.service_mapping,
            &resource_name,
            Self::GENERIC_SERVICE_KEY,
            &resource_name,
            "eventbridge",
            config.use_instance_service_names,
        );

        span.name = "aws.eventbridge".to_string();
        span.service = service_name;
        span.resource = resource_name;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.eventbridge".to_string()),
            ("detail_type".to_string(), self.detail_type.clone()),
        ]);
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        HashMap::from([
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "eventbridge".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                self.source.clone(),
            ),
        ])
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if let Ok(detail) = serde_json::from_value::<HashMap<String, Value>>(self.detail.clone()) {
            if let Some(carrier) = detail.get(DATADOG_CARRIER_KEY) {
                return serde_json::from_value(carrier.clone()).unwrap_or_default();
            }
        }
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}
