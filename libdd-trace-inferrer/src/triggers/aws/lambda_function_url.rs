// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS Lambda Function URL trigger.
//!
//! Very similar to API Gateway HTTP API v2.0 but with a `lambda-url` domain.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    lowercase_key, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
    FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
};
use crate::utils::{parameterize_api_resource, resolve_service_name, MS_TO_NS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LambdaFunctionUrlEvent {
    #[serde(default)]
    pub route_key: String,
    #[serde(default)]
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub domain_name: String,
    #[serde(default)]
    pub time_epoch: i64,
    pub http: RequestContextHttp,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContextHttp {
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub source_ip: String,
    #[serde(default)]
    pub user_agent: String,
}

impl LambdaFunctionUrlEvent {
    const GENERIC_SERVICE_KEY: &'static str = "lambda_url";

    fn service_id(&self) -> String {
        self.request_context.domain_name.clone()
    }
}

impl Trigger for LambdaFunctionUrlEvent {
    fn new(payload: Value) -> Option<Self> {
        serde_json::from_value(payload).ok()
    }

    fn is_match(payload: &Value) -> bool {
        let version = payload.get("version");
        let domain_name = payload
            .get("requestContext")
            .and_then(|rc| rc.get("domainName"));

        version.is_some_and(|v| v == "2.0")
            && payload.get("rawQueryString").is_some()
            && domain_name.is_some_and(|d| d.as_str().is_some_and(|s| s.contains("lambda-url")))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let resource = format!(
            "{} {}",
            self.request_context.http.method,
            parameterize_api_resource(self.request_context.http.path.clone())
        );
        let http_url = format!(
            "https://{}{}",
            self.request_context.domain_name, self.request_context.http.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = resolve_service_name(
            &config.service_mapping,
            &self.service_id(),
            Self::GENERIC_SERVICE_KEY,
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            config.use_instance_service_names,
        );

        span.name = "aws.lambda.url".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            (
                "endpoint".to_string(),
                self.request_context.http.path.clone(),
            ),
            ("http.url".to_string(), http_url),
            (
                "http.method".to_string(),
                self.request_context.http.method.clone(),
            ),
            (
                "http.protocol".to_string(),
                self.request_context.http.protocol.clone(),
            ),
            (
                "http.source_ip".to_string(),
                self.request_context.http.source_ip.clone(),
            ),
            (
                "http.user_agent".to_string(),
                self.request_context.http.user_agent.clone(),
            ),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
        ]);
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        HashMap::from([
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "lambda-function-url".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                self.request_context.domain_name.clone(),
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
