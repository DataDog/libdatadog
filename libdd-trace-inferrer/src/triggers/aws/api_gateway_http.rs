// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS API Gateway HTTP API (v2.0) trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger,
    lowercase_key,
};
use crate::utils::{
    MS_TO_NS, get_aws_partition_by_region, parameterize_api_resource, resolve_service_name,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayHttpEvent {
    #[serde(default)]
    pub route_key: String,
    #[serde(default)]
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(default)]
    pub path_parameters: HashMap<String, String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub query_string_parameters: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub api_id: String,
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

impl ApiGatewayHttpEvent {
    fn get_specific_service_id(&self) -> String {
        self.request_context.api_id.clone()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_api_gateway"
    }
}

impl Trigger for ApiGatewayHttpEvent {
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
            && domain_name
                .is_some_and(|d| d.as_str().is_none_or(|s| !s.contains("lambda-url")))
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
            &self.get_specific_service_id(),
            self.get_generic_service_id(),
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            config.use_instance_service_names,
        );

        span.name = "aws.httpapi".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("endpoint".to_string(), self.request_context.http.path.clone()),
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

    fn get_tags(&self, config: &InferConfig) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                format!(
                    "https://{}{}",
                    self.request_context.domain_name, self.request_context.http.path
                ),
            ),
            (
                "http.url_details.path".to_string(),
                self.request_context.http.path.clone(),
            ),
            (
                "http.method".to_string(),
                self.request_context.http.method.clone(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        if !self.route_key.is_empty() {
            if let Some(route) = self.route_key.split_whitespace().last() {
                tags.insert("http.route".to_string(), route.to_string());
            }
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
        tags.insert(
            FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
            arn,
        );

        // dd_resource_key tag
        if !self.request_context.api_id.is_empty() {
            let partition = get_aws_partition_by_region(&config.region);
            let dd_resource_key = format!(
                "arn:{partition}:apigateway:{region}::/apis/{api_id}",
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PAYLOAD: &str = r#"{
        "version": "2.0",
        "routeKey": "GET /httpapi/get",
        "rawPath": "/httpapi/get",
        "rawQueryString": "",
        "headers": {
            "User-Agent": "curl/7.64.1",
            "X-Forwarded-For": "38.122.226.210"
        },
        "requestContext": {
            "apiId": "x02yirxc7a",
            "domainName": "x02yirxc7a.execute-api.sa-east-1.amazonaws.com",
            "http": {
                "method": "GET",
                "path": "/httpapi/get",
                "protocol": "HTTP/1.1",
                "sourceIp": "38.122.226.210",
                "userAgent": "curl/7.64.1"
            },
            "requestId": "FaHnXjKCGjQEJ7A=",
            "stage": "$default",
            "timeEpoch": 1631212283738
        }
    }"#;

    #[test]
    fn test_is_match() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        assert!(ApiGatewayHttpEvent::is_match(&value));
    }

    #[test]
    fn test_new() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        let event = ApiGatewayHttpEvent::new(value).unwrap();
        assert_eq!(event.route_key, "GET /httpapi/get");
        assert_eq!(event.request_context.http.method, "GET");
    }

    #[test]
    fn test_enrich_span() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        let event = ApiGatewayHttpEvent::new(value).unwrap();
        let config = InferConfig::default();
        let mut span = SpanData::default();
        event.enrich_span(&mut span, &config);

        assert_eq!(span.name, "aws.httpapi");
        assert_eq!(
            span.service,
            "x02yirxc7a.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /httpapi/get");
        assert_eq!(span.r#type, "web");
    }

    #[test]
    fn test_get_tags() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        let event = ApiGatewayHttpEvent::new(value).unwrap();
        let config = InferConfig::default();
        let tags = event.get_tags(&config);
        assert_eq!(
            tags.get("function_trigger.event_source"),
            Some(&"api-gateway".to_string())
        );
        assert_eq!(
            tags.get("http.route"),
            Some(&"/httpapi/get".to_string())
        );
    }

    #[test]
    fn test_get_arn_via_tags() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        let event = ApiGatewayHttpEvent::new(value).unwrap();
        let mut config = InferConfig::default();
        config.region = "sa-east-1".to_string();
        let tags = event.get_tags(&config);
        assert_eq!(
            tags.get(FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG),
            Some(&"arn:aws:apigateway:sa-east-1::/restapis/x02yirxc7a/stages/$default".to_string())
        );
    }

    #[test]
    fn test_get_carrier_has_lowercased_headers() {
        let value: Value = serde_json::from_str(TEST_PAYLOAD).unwrap();
        let event = ApiGatewayHttpEvent::new(value).unwrap();
        let carrier = event.get_carrier();
        assert!(carrier.contains_key("user-agent"));
        assert!(!carrier.contains_key("User-Agent"));
    }
}
