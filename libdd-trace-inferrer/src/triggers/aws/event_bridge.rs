// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS EventBridge trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    Trigger, DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
    FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
};
use crate::utils::{resolve_service_name, MS_TO_NS};
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

pub(super) fn carrier_from_detail(detail: &Value) -> HashMap<String, String> {
    detail
        .as_object()
        .and_then(|detail| detail.get(DATADOG_CARRIER_KEY))
        .and_then(carrier_from_value)
        .unwrap_or_default()
}

pub(super) fn carrier_from_eventbridge_body(body: &str) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct EventBridgeEnvelope {
        detail: Option<Value>,
    }

    serde_json::from_str::<EventBridgeEnvelope>(body)
        .ok()
        .and_then(|envelope| envelope.detail.map(|detail| carrier_from_detail(&detail)))
        .unwrap_or_default()
}

fn carrier_from_value(value: &Value) -> Option<HashMap<String, String>> {
    match value {
        Value::Object(_) => serde_json::from_value(value.clone()).ok(),
        Value::String(s) => carrier_from_string(s),
        _ => None,
    }
}

fn carrier_from_string(s: &str) -> Option<HashMap<String, String>> {
    if let Ok(carrier) = serde_json::from_str::<HashMap<String, String>>(s) {
        return Some(carrier);
    }

    use base64::{engine::general_purpose::STANDARD, Engine};

    STANDARD
        .decode(s)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, String>>(&bytes).ok())
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
        carrier_from_detail(&self.detail)
    }

    fn is_async(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_carrier_from_datadog_object() {
        let event = event_with_datadog(json!({
            "traceparent": "00-11111111111111111111111111111111-2222222222222222-01",
            "x-datadog-start-time": "1740000000"
        }));

        assert_eq!(
            event.get_carrier(),
            HashMap::from([
                (
                    "traceparent".to_string(),
                    "00-11111111111111111111111111111111-2222222222222222-01".to_string()
                ),
                ("x-datadog-start-time".to_string(), "1740000000".to_string())
            ])
        );
    }

    #[test]
    fn test_get_carrier_from_datadog_json_string() {
        let event = event_with_datadog(Value::String(json_carrier()));

        assert_eq!(event.get_carrier(), expected_carrier());
    }

    #[test]
    fn test_get_carrier_from_datadog_base64_json_string() {
        let event = event_with_datadog(Value::String(base64_json_carrier()));

        assert_eq!(event.get_carrier(), expected_carrier());
    }

    #[test]
    fn test_carrier_from_transformed_eventbridge_body_with_base64_datadog() {
        let body = json!({
            "detail": {
                "message": "hello through sns eventbridge pipe",
                "_datadog": base64_json_carrier()
            }
        })
        .to_string();

        assert_eq!(carrier_from_eventbridge_body(&body), expected_carrier());
    }

    fn event_with_datadog(datadog: Value) -> EventBridgeEvent {
        EventBridgeEvent {
            id: "event-id".to_string(),
            version: "0".to_string(),
            account: "123456789012".to_string(),
            time: "2026-08-04T00:00:00Z".to_string(),
            region: "us-east-1".to_string(),
            resources: vec![],
            source: "rust-sqs-sample.client".to_string(),
            detail_type: "WorkItemSubmitted".to_string(),
            detail: json!({
                "message": "hello through eventbridge",
                "_datadog": datadog
            }),
            replay_name: None,
        }
    }

    fn json_carrier() -> String {
        serde_json::to_string(&expected_carrier()).unwrap()
    }

    fn base64_json_carrier() -> String {
        use base64::{engine::general_purpose::STANDARD, Engine};

        STANDARD.encode(json_carrier())
    }

    fn expected_carrier() -> HashMap<String, String> {
        HashMap::from([(
            "traceparent".to_string(),
            "00-11111111111111111111111111111111-2222222222222222-01".to_string(),
        )])
    }
}
