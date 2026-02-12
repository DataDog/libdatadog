// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS S3 trigger.

use crate::config::InferConfig;
use crate::span_data::SpanData;
use crate::span_pointer::{SpanPointer, generate_span_pointer_hash};
use crate::triggers::{FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger};
use crate::utils::{resolve_service_name, MS_TO_NS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct S3Record {
    #[serde(rename = "eventSource")]
    pub event_source: String,
    #[serde(rename = "eventTime")]
    pub event_time: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    pub s3: S3Entity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct S3Entity {
    pub bucket: S3Bucket,
    pub object: S3Object,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct S3Bucket {
    pub name: String,
    pub arn: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct S3Object {
    pub key: String,
    pub size: i64,
    #[serde(rename = "eTag")]
    pub e_tag: String,
}

impl Trigger for S3Record {
    fn new(payload: Value) -> Option<Self> {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|first| serde_json::from_value::<S3Record>(first.clone()).ok())
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("s3"))
            .is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut SpanData, config: &InferConfig) {
        let bucket_name = self.get_specific_service_id();
        let start_time = chrono::DateTime::parse_from_rfc3339(&self.event_time)
            .map(|dt| {
                dt.timestamp_nanos_opt()
                    .unwrap_or((dt.timestamp_millis() as f64 * MS_TO_NS) as i64)
            })
            .unwrap_or(0);

        let service_name = resolve_service_name(
            &config.service_mapping,
            &bucket_name,
            self.get_generic_service_id(),
            &bucket_name,
            "s3",
            config.use_instance_service_names,
        );

        span.name = "aws.s3".to_string();
        span.service = service_name;
        span.resource.clone_from(&bucket_name);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.s3".to_string()),
            ("event_name".to_string(), self.event_name.clone()),
            ("bucketname".to_string(), bucket_name),
            ("bucket_arn".to_string(), self.s3.bucket.arn.clone()),
            ("object_key".to_string(), self.s3.object.key.clone()),
            ("object_size".to_string(), self.s3.object.size.to_string()),
            ("object_etag".to_string(), self.s3.object.e_tag.clone()),
        ]);
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "s3".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.event_source.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_specific_service_id(&self) -> String {
        self.s3.bucket.name.clone()
    }

    fn get_generic_service_id(&self) -> &'static str {
        "lambda_s3"
    }

    fn get_span_pointers(&self) -> Option<Vec<SpanPointer>> {
        let bucket_name = &self.s3.bucket.name;
        let key = &self.s3.object.key;
        let e_tag = self.s3.object.e_tag.trim_matches('"');

        if bucket_name.is_empty() || key.is_empty() || e_tag.is_empty() {
            debug!("Unable to create span pointer: bucket name, key, or etag is missing");
            return None;
        }

        let hash = generate_span_pointer_hash(&[bucket_name, key, e_tag]);
        Some(vec![SpanPointer {
            hash,
            kind: "aws.s3.object".to_string(),
        }])
    }
}
