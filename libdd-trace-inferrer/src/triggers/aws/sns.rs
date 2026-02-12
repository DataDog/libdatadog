// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS SNS trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger};
use crate::utils::{resolve_service_name, MS_TO_NS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::event_bridge::EventBridgeEvent;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsRecord {
    #[serde(rename = "Sns")]
    pub sns: SnsEntity,
    #[serde(rename = "EventSubscriptionArn")]
    pub event_subscription_arn: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsEntity {
    #[serde(rename = "MessageId")]
    pub message_id: String,
    #[serde(rename = "Type")]
    pub r#type: String,
    #[serde(rename = "TopicArn")]
    pub topic_arn: String,
    #[serde(rename = "MessageAttributes")]
    pub message_attributes: HashMap<String, SnsMessageAttribute>,
    #[serde(rename = "Timestamp")]
    pub timestamp: String,
    #[serde(rename = "Subject")]
    pub subject: Option<String>,
    #[serde(rename = "Message")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsMessageAttribute {
    #[serde(rename = "Type")]
    pub r#type: String,
    #[serde(rename = "Value")]
    pub value: String,
}

impl Trigger for SnsRecord {
    fn new(payload: Value) -> Option<Self> {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|first| serde_json::from_value::<SnsRecord>(first.clone()).ok())
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("Sns"))
            .is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let resource_name = self.get_specific_service_id();

        // Parse ISO 8601 timestamp to nanoseconds
        let start_time = chrono::DateTime::parse_from_rfc3339(&self.sns.timestamp)
            .map(|dt| {
                dt.timestamp_nanos_opt()
                    .unwrap_or((dt.timestamp_millis() as f64 * MS_TO_NS) as i64)
            })
            .unwrap_or(0);

        let service_name = resolve_service_name(
            &config.service_mapping,
            &self.get_specific_service_id(),
            self.get_generic_service_id(),
            &self.get_specific_service_id(),
            "sns",
            config.use_instance_service_names,
        );

        span.name = "aws.sns".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource_name);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.sns".to_string()),
            ("topicname".to_string(), resource_name),
            ("topic_arn".to_string(), self.sns.topic_arn.clone()),
            ("message_id".to_string(), self.sns.message_id.clone()),
            ("type".to_string(), self.sns.r#type.clone()),
        ]);

        if let Some(subject) = &self.sns.subject {
            span.meta.insert("subject".to_string(), subject.clone());
        }

        if let Some(event_subscription_arn) = &self.event_subscription_arn {
            span.meta.insert(
                "event_subscription_arn".to_string(),
                event_subscription_arn.clone(),
            );
        }
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "sns".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.sns.topic_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if let Some(ma) = self.sns.message_attributes.get(DATADOG_CARRIER_KEY) {
            match ma.r#type.as_str() {
                "String" => return serde_json::from_str(&ma.value).unwrap_or_default(),
                "Binary" => {
                    // base64 decode then parse as JSON carrier
                    use base64::{Engine, engine::general_purpose::STANDARD};
                    if let Ok(bytes) = STANDARD.decode(&ma.value) {
                        if let Ok(carrier_str) = String::from_utf8(bytes) {
                            return serde_json::from_str(&carrier_str).unwrap_or_default();
                        }
                    }
                }
                _ => {
                    debug!("Unsupported type in SNS message attribute");
                }
            }
        }

        // Fallback: check message for EventBridge event
        if let Some(message) = &self.sns.message {
            if let Ok(event) = serde_json::from_str::<EventBridgeEvent>(message) {
                return event.get_carrier();
            }
        }

        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_specific_service_id(&self) -> String {
        self.sns
            .topic_arn
            .split(':')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_sns"
    }
}

/// Returns the wrapped trigger if SNS message contains EventBridge.
impl SnsRecord {
    pub fn get_wrapped_trigger(&self) -> Option<EventBridgeEvent> {
        self.sns
            .message
            .as_ref()
            .and_then(|msg| serde_json::from_str::<EventBridgeEvent>(msg).ok())
    }
}
