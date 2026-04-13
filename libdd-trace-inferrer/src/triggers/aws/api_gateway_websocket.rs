// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS API Gateway WebSocket trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger,
    lowercase_key,
};
use crate::utils::{MS_TO_NS, get_aws_partition_by_region, resolve_service_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayWebSocketEvent {
    #[serde(default)]
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub stage: String,
    pub request_id: String,
    pub api_id: String,
    pub domain_name: String,
    #[serde(default)]
    pub connection_id: String,
    #[serde(default)]
    pub route_key: String,
    #[serde(rename = "requestTimeEpoch")]
    #[serde(default)]
    pub time_epoch: i64,
    #[serde(default)]
    pub message_direction: String,
}

impl ApiGatewayWebSocketEvent {
    const GENERIC_SERVICE_KEY: &'static str = "lambda_api_gateway";

    fn service_id(&self) -> String {
        self.request_context.api_id.clone()
    }
}

impl Trigger for ApiGatewayWebSocketEvent {
    fn new(payload: Value) -> Option<Self> {
        serde_json::from_value(payload).ok()
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("requestContext")
            .and_then(|rc| rc.get("connectionId"))
            .is_some()
            && payload
                .get("requestContext")
                .and_then(|rc| rc.get("routeKey"))
                .is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = resolve_service_name(
            &config.service_mapping,
            &self.service_id(),
            Self::GENERIC_SERVICE_KEY,
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            config.use_instance_service_names,
        );

        span.name = "aws.apigateway.websocket".to_string();
        span.service = service_name;
        span.resource = self.request_context.route_key.clone();
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            (
                "connection_id".to_string(),
                self.request_context.connection_id.clone(),
            ),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
            (
                "message_direction".to_string(),
                self.request_context.message_direction.clone(),
            ),
        ]);
    }

    fn get_tags(&self, config: &InferConfig) -> HashMap<String, String> {
        let partition = get_aws_partition_by_region(&config.region);
        let arn = format!(
            "arn:{partition}:apigateway:{region}::/restapis/{api_id}/stages/{stage}",
            region = config.region,
            api_id = self.request_context.api_id,
            stage = self.request_context.stage,
        );

        HashMap::from([
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                arn,
            ),
        ])
    }

    fn is_async(&self) -> bool {
        self.headers
            .get("x-amz-invocation-type")
            .is_some_and(|v| v == "Event")
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        self.headers.clone()
    }
}
