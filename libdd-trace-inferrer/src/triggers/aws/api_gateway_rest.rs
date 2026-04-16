// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS API Gateway REST API (v1.0) trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::serde_utils::nullable_map;
use crate::triggers::{
    lowercase_key, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
    FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
};
use crate::utils::{
    get_aws_partition_by_region, parameterize_api_resource, resolve_service_name, MS_TO_NS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayRestEvent {
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(deserialize_with = "lowercase_key")]
    pub multi_value_headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    #[serde(deserialize_with = "nullable_map")]
    #[serde(rename = "multiValueQueryStringParameters")]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub query_parameters: HashMap<String, Vec<String>>,
    #[serde(default)]
    #[serde(deserialize_with = "nullable_map")]
    pub path_parameters: HashMap<String, String>,
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub stage: String,
    pub request_id: String,
    pub api_id: String,
    pub domain_name: String,
    #[serde(rename = "requestTimeEpoch")]
    pub time_epoch: i64,
    #[serde(rename = "httpMethod")]
    pub method: String,
    pub resource_path: String,
    pub path: String,
    pub protocol: String,
    pub identity: Identity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub source_ip: String,
    pub user_agent: String,
}

impl ApiGatewayRestEvent {
    fn get_specific_service_id(&self) -> String {
        self.request_context.api_id.clone()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_api_gateway"
    }
}

impl Trigger for ApiGatewayRestEvent {
    fn new(payload: Value) -> Option<Self> {
        serde_json::from_value(payload).ok()
    }

    fn is_match(payload: &Value) -> bool {
        let stage = payload.get("requestContext").and_then(|v| v.get("stage"));
        let http_method = payload.get("httpMethod");
        let resource = payload.get("resource");
        stage.is_some() && http_method.is_some() && resource.is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let resource = format!(
            "{} {}",
            self.request_context.method,
            parameterize_api_resource(self.request_context.path.clone())
        );
        let http_url = format!(
            "https://{}{}",
            self.request_context.domain_name, self.request_context.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = resolve_service_name(
            &config.service_mapping,
            &self.get_specific_service_id(),
            self.get_generic_service_id(),
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            config.use_instance_service_names,
        );

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("endpoint".to_string(), self.request_context.path.clone()),
            ("http.url".to_string(), http_url),
            (
                "http.method".to_string(),
                self.request_context.method.clone(),
            ),
            (
                "http.protocol".to_string(),
                self.request_context.protocol.clone(),
            ),
            (
                "http.source_ip".to_string(),
                self.request_context.identity.source_ip.clone(),
            ),
            (
                "http.user_agent".to_string(),
                self.request_context.identity.user_agent.clone(),
            ),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
            (
                "resource_path".to_string(),
                self.request_context.resource_path.clone(),
            ),
            ("stage".to_string(), self.request_context.stage.clone()),
        ]);
    }

    fn get_tags(&self, config: &InferConfig) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                format!(
                    "https://{}{}",
                    self.request_context.domain_name, self.request_context.path
                ),
            ),
            (
                "http.url_details.path".to_string(),
                self.request_context.path.clone(),
            ),
            (
                "http.method".to_string(),
                self.request_context.method.clone(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        if !self.request_context.resource_path.is_empty() {
            tags.insert(
                "http.route".to_string(),
                self.request_context.resource_path.clone(),
            );
        }

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.to_string());
        }

        if let Some(user_agent) = self.headers.get("user-agent") {
            tags.insert("http.user_agent".to_string(), user_agent.to_string());
        }

        // ARN tag
        let partition = get_aws_partition_by_region(&config.region);
        let arn = format!(
            "arn:{partition}:apigateway:{region}::/restapis/{api_id}/stages/{stage}",
            region = config.region,
            api_id = self.request_context.api_id,
            stage = self.request_context.stage,
        );
        tags.insert(FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(), arn);

        // dd_resource_key tag
        if !self.request_context.api_id.is_empty() {
            let partition = get_aws_partition_by_region(&config.region);
            let dd_resource_key = format!(
                "arn:{partition}:apigateway:{region}::/restapis/{api_id}",
                region = config.region,
                api_id = self.request_context.api_id,
            );
            tags.insert("dd_resource_key".to_string(), dd_resource_key);
        }

        tags
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
