// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS Application Load Balancer trigger.
//!
//! ALB events do NOT produce an inferred span. They only contribute trigger
//! tags and a carrier for trace context extraction.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger,
    lowercase_key,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlbEvent {
    pub request_context: RequestContext,
    pub http_method: String,
    pub path: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub multi_value_headers: HashMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContext {
    pub elb: Elb,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Elb {
    pub target_group_arn: String,
}

impl Trigger for AlbEvent {
    fn new(payload: Value) -> Option<Self> {
        serde_json::from_value(payload).ok()
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("requestContext")
            .and_then(|v| v.get("elb"))
            .and_then(|v| v.get("targetGroupArn"))
            .is_some()
    }

    fn enrich_span(&self, _span: &mut SpanData, _config: &InferConfig) {
        // ALB events do not produce inferred spans.
    }

    fn get_tags(&self, _config: &InferConfig) -> HashMap<String, String> {
        HashMap::from([
            ("http.method".to_string(), self.http_method.clone()),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "alb".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                self.request_context.elb.target_group_arn.clone(),
            ),
        ])
    }

    fn is_async(&self) -> bool {
        false
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if !self.headers.is_empty() {
            return self.headers.clone();
        }
        // For multi-value headers, take the first value of each
        self.multi_value_headers
            .iter()
            .filter_map(|(k, v)| v.first().map(|first| (k.to_lowercase(), first.clone())))
            .collect()
    }
}
