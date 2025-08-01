// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This module should only ever be used in test code. Relaxing the crate level clippy lints to warn
// when panic macros are used.
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::todo)]
#![allow(clippy::unimplemented)]

pub mod datadog_test_agent;

use std::collections::HashMap;
use std::time::Duration;

use crate::send_data::SendData;
use crate::span::SpanBytes;
use crate::span::{v05, SharedDictBytes};
use crate::trace_utils::TracerHeaderTags;
use crate::tracer_payload::TracerPayloadCollection;
use datadog_trace_protobuf::pb;
use ddcommon::Endpoint;
use httpmock::Mock;
use serde_json::json;
use tinybytes::BytesString;
use tokio::time::sleep;

pub fn create_test_no_alloc_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
) -> SpanBytes {
    let mut span = SpanBytes {
        trace_id,
        span_id,
        service: BytesString::from_slice("test-service".as_ref()).unwrap(),
        name: BytesString::from_slice("test_name".as_ref()).unwrap(),
        resource: BytesString::from_slice("test-resource".as_ref()).unwrap(),
        parent_id,
        start,
        duration: 5,
        error: 0,
        meta: HashMap::from([
            (
                BytesString::from_slice("service".as_ref()).unwrap(),
                BytesString::from_slice("test-service".as_ref()).unwrap(),
            ),
            (
                BytesString::from_slice("env".as_ref()).unwrap(),
                BytesString::from_slice("test-env".as_ref()).unwrap(),
            ),
            (
                BytesString::from_slice("runtime-id".as_ref()).unwrap(),
                BytesString::from_slice("test-runtime-id-value".as_ref()).unwrap(),
            ),
        ]),
        metrics: HashMap::new(),
        r#type: BytesString::default(),
        meta_struct: HashMap::new(),
        span_links: vec![],
        span_events: vec![],
    };
    if is_top_level {
        span.metrics
            .insert(BytesString::from_slice("_top_level".as_ref()).unwrap(), 1.0);
        span.meta.insert(
            BytesString::from_slice("_dd.origin".as_ref()).unwrap(),
            BytesString::from_slice("cloudfunction".as_ref()).unwrap(),
        );
        span.meta.insert(
            BytesString::from_slice("origin".as_ref()).unwrap(),
            BytesString::from_slice("cloudfunction".as_ref()).unwrap(),
        );
        span.meta.insert(
            BytesString::from_slice("functionname".as_ref()).unwrap(),
            BytesString::from_slice("dummy_function_name".as_ref()).unwrap(),
        );
        span.r#type = BytesString::from_slice("serverless".as_ref()).unwrap();
    }
    span
}

pub fn create_test_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
) -> pb::Span {
    let mut span = pb::Span {
        trace_id,
        span_id,
        service: "test-service".to_string(),
        name: "test_name".to_string(),
        resource: "test-resource".to_string(),
        parent_id,
        start,
        duration: 5,
        error: 0,
        meta: HashMap::from([
            ("service".to_string(), "test-service".to_string()),
            ("env".to_string(), "test-env".to_string()),
            (
                "runtime-id".to_string(),
                "test-runtime-id-value".to_string(),
            ),
        ]),
        metrics: HashMap::new(),
        r#type: "".to_string(),
        meta_struct: HashMap::new(),
        span_links: vec![],
    };
    if is_top_level {
        span.metrics.insert("_top_level".to_string(), 1.0);
        span.meta
            .insert("_dd.origin".to_string(), "cloudfunction".to_string());
        span.meta
            .insert("origin".to_string(), "cloudfunction".to_string());
        span.meta.insert(
            "functionname".to_string(),
            "dummy_function_name".to_string(),
        );
    }
    span
}

pub fn create_test_gcp_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
) -> pb::Span {
    let mut span = pb::Span {
        trace_id,
        span_id,
        service: "test-service".to_string(),
        name: "test_name".to_string(),
        resource: "test-resource".to_string(),
        parent_id,
        start,
        duration: 5,
        error: 0,
        meta: HashMap::from([
            ("service".to_string(), "test-service".to_string()),
            ("env".to_string(), "test-env".to_string()),
            (
                "runtime-id".to_string(),
                "test-runtime-id-value".to_string(),
            ),
        ]),
        metrics: HashMap::new(),
        r#type: "".to_string(),
        meta_struct: HashMap::new(),
        span_links: vec![],
    };
    span.meta.insert(
        "_dd.serverless_compat_version".to_string(),
        "dummy_version".to_string(),
    );
    span.meta.insert(
        "gcrfx.project_id".to_string(),
        "dummy_project_id".to_string(),
    );
    span.meta.insert(
        "gcrfx.location".to_string(),
        "dummy_region_west".to_string(),
    );
    span.meta.insert(
        "gcrfx.resource_name".to_string(),
        "projects/dummy_project_id/locations/dummy_region_west/functions/dummy_function_name"
            .to_string(),
    );
    if is_top_level {
        span.meta.insert(
            "functionname".to_string(),
            "dummy_function_name".to_string(),
        );
        span.metrics.insert("_top_level".to_string(), 1.0);
        span.meta
            .insert("_dd.origin".to_string(), "cloudfunction".to_string());
        span.meta
            .insert("origin".to_string(), "cloudfunction".to_string());
    }
    span
}

pub fn create_test_gcp_json_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
) -> serde_json::Value {
    json!(
        {
            "trace_id": trace_id,
            "span_id": span_id,
            "service": "test-service",
            "name": "test_name",
            "resource": "test-resource",
            "parent_id": parent_id,
            "start": start,
            "duration": 5,
            "error": 0,
            "meta": {
                "service": "test-service",
                "env": "test-env",
                "runtime-id": "test-runtime-id-value",
                "gcrfx.project_id": "dummy_project_id",
                "_dd.serverless_compat_version": "dummy_version",
                "gcrfx.resource_name": "projects/dummy_project_id/locations/dummy_region_west/functions/dummy_function_name",
                "gcrfx.location": "dummy_region_west"
            },
            "metrics": {},
            "meta_struct": {},
            "span_links": [],
        }
    )
}

pub fn create_test_v05_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
    dict: &mut SharedDictBytes,
    metrics: Option<Vec<(String, f64)>>,
) -> v05::Span {
    let mut meta = HashMap::from([
        (
            dict.get_or_insert(&BytesString::from("service")).unwrap(),
            dict.get_or_insert(&BytesString::from("test-service"))
                .unwrap(),
        ),
        (
            dict.get_or_insert(&BytesString::from("env")).unwrap(),
            dict.get_or_insert(&BytesString::from("test-env")).unwrap(),
        ),
        (
            dict.get_or_insert(&BytesString::from("runtime-id"))
                .unwrap(),
            dict.get_or_insert(&BytesString::from("test-runtime-id-value"))
                .unwrap(),
        ),
    ]);

    if is_top_level {
        meta.extend([
            (
                dict.get_or_insert(&BytesString::from("functionname"))
                    .unwrap(),
                dict.get_or_insert(&BytesString::from("dummy_function_name"))
                    .unwrap(),
            ),
            (
                dict.get_or_insert(&BytesString::from("_dd.origin"))
                    .unwrap(),
                dict.get_or_insert(&BytesString::from("cloudfunction"))
                    .unwrap(),
            ),
            (
                dict.get_or_insert(&BytesString::from("origin")).unwrap(),
                dict.get_or_insert(&BytesString::from("cloudfunction"))
                    .unwrap(),
            ),
        ]);
    }
    v05::Span {
        service: dict
            .get_or_insert(&BytesString::from("test-service"))
            .unwrap(),
        name: dict.get_or_insert(&BytesString::from("test_name")).unwrap(),
        resource: dict
            .get_or_insert(&BytesString::from("test-resource"))
            .unwrap(),
        trace_id,
        span_id,
        parent_id,
        start,
        duration: 5,
        error: 0,
        meta,
        metrics: if let Some(metrics) = metrics {
            metrics
                .into_iter()
                .map(|(k, v)| (dict.get_or_insert(&BytesString::from(k)).unwrap(), v))
                .collect()
        } else {
            HashMap::new()
        },
        r#type: if is_top_level {
            dict.get_or_insert(&BytesString::from("web")).unwrap()
        } else {
            dict.get_or_insert(&BytesString::from("")).unwrap()
        },
    }
}

pub fn create_test_json_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
) -> serde_json::Value {
    let mut span = json!(
        {
            "trace_id": trace_id,
            "span_id": span_id,
            "service": "test-service",
            "name": "test_name",
            "resource": "test-resource",
            "parent_id": parent_id,
            "start": start,
            "duration": 5,
            "error": 0,
            "meta": {
                "service": "test-service",
                "env": "test-env",
                "runtime-id": "test-runtime-id-value",
            },
            "metrics": {},
            "meta_struct": {},
            "span_links": [],
            "span_events": [],
        }
    );

    if is_top_level {
        let additional_meta = json!(
            {
                "functionname": "dummy_function_name",
                "_dd.origin": "cloudfunction",
                "origin": "cloudfunction",
            }
        );
        span.get_mut("meta")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .extend(additional_meta.as_object().unwrap().clone());

        span["type"] = json!("serverless");

        span["metrics"] = json!(
            {
                "_top_level": 1.0,
            }
        );
    }

    span
}

/// This is a helper function for observing if a httpmock object has been "hit" the expected number
/// of times. If not it will perform a tokio::sleep and try again. If `delete_after_hits` is set to
/// true it will delete the mock. More attempts at lower sleep intervals is preferred to reduce
/// flakiness and test runtime. This is especially useful when testing async code that may not block
/// on receiving a response.
///
/// # Arguments
///
/// * `mock` - A mutable reference to the Mock object.
/// * `poll_attempts` - The number of times to attempt polling the mock server.
/// * `sleep_interval_ms` - The interval in milliseconds to sleep between each poll attempt.
/// * `expected_hits` - The expected number of hits on the mock server.
/// * `delete_after_hits` - A boolean indicating whether to delete the mock after a hit is observed.
///
/// # Returns
///
/// * A boolean indicating whether the expected number of hits was observed on the mock.
///
/// # Examples
///
/// ```
/// use datadog_trace_utils::test_utils::poll_for_mock_hit;
/// use httpmock::MockServer;
///
/// #[cfg_attr(miri, ignore)]
/// async fn test_with_poll() {
///     let server = MockServer::start();
///
///     let mut mock = server
///         .mock_async(|_when, then| {
///             then.status(202)
///                 .header("content-type", "application/json")
///                 .body(r#"{"status":"ok"}"#);
///         })
///         .await;
///
///     // Do something that would trigger a request to the mock server
///
///     assert!(
///         poll_for_mock_hit(&mut mock, 10, 100, 1, true).await,
///         "Expected a request"
///     );
/// }
/// ```
pub async fn poll_for_mock_hit(
    mock: &mut Mock<'_>,
    poll_attempts: i32,
    sleep_interval_ms: u64,
    expected_hits: usize,
    delete_after_hits: bool,
) -> bool {
    let mut mock_hit = false;

    let mut mock_observations_remaining = poll_attempts;

    while !mock_hit {
        sleep(Duration::from_millis(sleep_interval_ms)).await;
        mock_observations_remaining -= 1;
        mock_hit = if expected_hits > 0 {
            mock.hits_async().await == expected_hits
        } else {
            // If we are polling for 0 hits, we need to ensure we do all observations, otherwise
            // this will always be true
            mock.hits_async().await == 0 && mock_observations_remaining == 0
        };

        if mock_observations_remaining == 0 || mock_hit {
            if delete_after_hits {
                mock.delete();
            }
            break;
        }
    }

    mock_hit
}

/// Creates a `SendData` object with the specified size and target endpoint.
///
/// This function is a test helper to create a `SendData` object.
/// The `SendData` object is initialized with a default `TracerHeaderTags` object and a
/// `TracerPayload` object with predefined values.
///
/// # Arguments
///
/// * `size` - The size of the data to be sent.
/// * `target_endpoint` - A reference to the `Endpoint` object where the data will be sent.
///
/// # Returns
///
/// * A `SendData` object.
///
/// # Examples
///
/// ```
/// use datadog_trace_utils::test_utils::create_send_data;
/// use ddcommon::Endpoint;
///
/// let size = 512;
/// let target_endpoint = Endpoint {
///     url: "http://localhost:8080".to_owned().parse().unwrap(),
///     api_key: Some("test-key".into()),
///     ..Default::default()
/// };
///
/// let send_data = create_send_data(size, &target_endpoint);
/// ```
// TODO: When necessary this can take in a TracerPayload object to better customize the payload
pub fn create_send_data(size: usize, target_endpoint: &Endpoint) -> SendData {
    let tracer_header_tags = TracerHeaderTags::default();

    let tracer_payload = pb::TracerPayload {
        container_id: "container_id_1".to_owned(),
        language_name: "php".to_owned(),
        language_version: "4.0".to_owned(),
        tracer_version: "1.1".to_owned(),
        runtime_id: "runtime_1".to_owned(),
        chunks: vec![],
        tags: Default::default(),
        env: "test".to_owned(),
        hostname: "test_host".to_owned(),
        app_version: "2.0".to_owned(),
    };

    SendData::new(
        size,
        TracerPayloadCollection::V07(vec![tracer_payload]),
        tracer_header_tags,
        target_endpoint,
    )
}
