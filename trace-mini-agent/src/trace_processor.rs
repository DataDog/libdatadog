// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::sync::Arc;

use async_trait::async_trait;
use hyper::{http, Body, Request, Response, StatusCode};
use log::error;
use tokio::sync::mpsc::Sender;

use datadog_trace_normalization::normalizer;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils;

use crate::{
    config::Config,
    http_utils::{self, log_and_create_http_response},
};

/// Used to populate root_span_tags fields if they exist in the root span's meta tags
macro_rules! parse_root_span_tags {
    (
        $root_span_meta_map:ident,
        { $($tag:literal => $($root_span_tags_struct_field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(root_span_tag_value) = $root_span_meta_map.get($tag) {
                $($root_span_tags_struct_field).+ = root_span_tag_value;
            }
        )+
    }
}

#[async_trait]
pub trait TraceProcessor {
    /// Deserializes traces from a hyper request body and sends them through the provided tokio mpsc Sender.
    async fn process_traces(
        &self,
        config: Arc<Config>,
        req: Request<Body>,
        tx: Sender<pb::TracerPayload>,
        mini_agent_metadata: Arc<trace_utils::MiniAgentMetadata>,
    ) -> http::Result<Response<Body>>;
}

#[derive(Clone)]
pub struct ServerlessTraceProcessor {}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(
        &self,
        config: Arc<Config>,
        req: Request<Body>,
        tx: Sender<pb::TracerPayload>,
        mini_agent_metadata: Arc<trace_utils::MiniAgentMetadata>,
    ) -> http::Result<Response<Body>> {
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            config.max_request_content_length,
            "Error processing traces",
        ) {
            return response;
        }

        let tracer_header_tags = trace_utils::get_tracer_header_tags(&parts.headers);

        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf crate)
        let mut traces = match trace_utils::get_traces_from_request_body(body).await {
            Ok(res) => res,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error deserializing trace from request body: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let mut trace_chunks: Vec<pb::TraceChunk> = Vec::new();

        let mut gathered_root_span_tags = false;
        let mut root_span_tags = trace_utils::RootSpanTags::default();

        for trace in traces.iter_mut() {
            if let Err(e) = normalizer::normalize_trace(trace) {
                error!("Error normalizing trace: {e}");
            }

            let mut chunk = trace_utils::construct_trace_chunk(trace.to_vec());

            let root_span_index = match trace_utils::get_root_span_index(trace) {
                Ok(res) => res,
                Err(e) => {
                    error!("Error getting the root span index of a trace, skipping. {e}");
                    continue;
                }
            };

            if let Err(e) = normalizer::normalize_chunk(&mut chunk, root_span_index) {
                error!("Error normalizing trace chunk: {e}");
            }

            for span in chunk.spans.iter_mut() {
                // TODO: obfuscate & truncate spans
                if tracer_header_tags.client_computed_top_level {
                    trace_utils::update_tracer_top_level(span);
                }
                trace_utils::enrich_span_with_mini_agent_metadata(span, &mini_agent_metadata);
            }

            if !tracer_header_tags.client_computed_top_level {
                trace_utils::compute_top_level_span(&mut chunk.spans);
            }

            trace_utils::set_serverless_root_span_tags(
                &mut chunk.spans[root_span_index],
                config.gcp_function_name.clone(),
            );

            trace_chunks.push(chunk);

            if !gathered_root_span_tags {
                gathered_root_span_tags = true;
                let meta_map = &trace[root_span_index].meta;
                parse_root_span_tags!(
                    meta_map,
                    {
                        "env" => root_span_tags.env,
                        "version" => root_span_tags.app_version,
                        "_dd.hostname" => root_span_tags.hostname,
                        "runtime-id" => root_span_tags.runtime_id,
                    }
                );
            }
        }

        let tracer_payload =
            trace_utils::construct_tracer_payload(trace_chunks, tracer_header_tags, root_span_tags);

        // send trace payload to our trace flusher
        match tx.send(tracer_payload).await {
            Ok(_) => {
                return log_and_create_http_response(
                    "Successfully buffered traces to be flushed.",
                    StatusCode::ACCEPTED,
                );
            }
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error sending traces to the trace flusher: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use hyper::Request;
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::mpsc::{self, Receiver, Sender};

    use crate::{
        config::Config,
        trace_processor::{self, TraceProcessor},
    };
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::trace_utils;

    fn create_test_span(start: i64, span_id: u64, parent_id: u64, is_top_level: bool) -> pb::Span {
        let mut span = pb::Span {
            trace_id: 111,
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
                    "afjksdljfkllksdj-28934889".to_string(),
                ),
            ]),
            metrics: HashMap::new(),
            r#type: "".to_string(),
            meta_struct: HashMap::new(),
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
            span.r#type = "serverless".to_string();
        }
        span
    }

    fn create_test_json_span(start: i64, span_id: u64, parent_id: u64) -> serde_json::Value {
        json!(
            {
                "trace_id": 111,
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
                    "runtime-id": "afjksdljfkllksdj-28934889",
                },
                "metrics": {},
                "meta_struct": {},
            }
        )
    }

    fn get_current_timestamp_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    fn create_test_config() -> Config {
        Config {
            api_key: "dummy_api_key".to_string(),
            gcp_function_name: Some("dummy_function_name".to_string()),
            max_request_content_length: 10 * 1024 * 1024,
            trace_flush_interval: 3,
            stats_flush_interval: 3,
            verify_env_timeout: 100,
            trace_intake_url: "trace.agent.notdog.com/traces".to_string(),
            trace_stats_intake_url: "trace.agent.notdog.com/stats".to_string(),
            dd_site: "datadoghq.com".to_string(),
        }
    }

    #[tokio::test]
    async fn test_process_trace() {
        let (tx, mut rx): (Sender<pb::TracerPayload>, Receiver<pb::TracerPayload>) =
            mpsc::channel(1);

        let start = get_current_timestamp_nanos();

        let json_span = create_test_json_span(start, 222, 0);

        let bytes = rmp_serde::to_vec(&vec![vec![json_span]]).unwrap();
        let request = Request::builder()
            .header("datadog-meta-tracer-version", "4.0.0")
            .header("datadog-meta-lang", "nodejs")
            .header("datadog-meta-lang-version", "v19.7.0")
            .header("datadog-meta-lang-interpreter", "v8")
            .header("datadog-container-id", "33")
            .header("content-length", "100")
            .body(hyper::body::Body::from(bytes))
            .unwrap();

        let trace_processor = trace_processor::ServerlessTraceProcessor {};
        let res = trace_processor
            .process_traces(
                Arc::new(create_test_config()),
                request,
                tx,
                Arc::new(trace_utils::MiniAgentMetadata::default()),
            )
            .await;
        assert!(res.is_ok());

        let tracer_payload = rx.recv().await;

        assert!(tracer_payload.is_some());

        let expected_tracer_payload = pb::TracerPayload {
            container_id: "33".to_string(),
            language_name: "nodejs".to_string(),
            language_version: "v19.7.0".to_string(),
            tracer_version: "4.0.0".to_string(),
            runtime_id: "afjksdljfkllksdj-28934889".to_string(),
            chunks: vec![pb::TraceChunk {
                priority: i8::MIN as i32,
                origin: "".to_string(),
                spans: vec![create_test_span(start, 222, 0, true)],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: HashMap::new(),
            env: "test-env".to_string(),
            hostname: "".to_string(),
            app_version: "".to_string(),
        };

        assert_eq!(expected_tracer_payload, tracer_payload.unwrap());
    }

    #[tokio::test]
    async fn test_process_trace_top_level_span_set() {
        let (tx, mut rx): (Sender<pb::TracerPayload>, Receiver<pb::TracerPayload>) =
            mpsc::channel(1);

        let start = get_current_timestamp_nanos();

        let json_trace = vec![
            create_test_json_span(start, 333, 222),
            create_test_json_span(start, 222, 0),
            create_test_json_span(start, 444, 333),
        ];

        let bytes = rmp_serde::to_vec(&vec![json_trace]).unwrap();
        let request = Request::builder()
            .header("datadog-meta-tracer-version", "4.0.0")
            .header("datadog-meta-lang", "nodejs")
            .header("datadog-meta-lang-version", "v19.7.0")
            .header("datadog-meta-lang-interpreter", "v8")
            .header("datadog-container-id", "33")
            .header("content-length", "100")
            .body(hyper::body::Body::from(bytes))
            .unwrap();

        let trace_processor = trace_processor::ServerlessTraceProcessor {};
        let res = trace_processor
            .process_traces(
                Arc::new(create_test_config()),
                request,
                tx,
                Arc::new(trace_utils::MiniAgentMetadata::default()),
            )
            .await;
        assert!(res.is_ok());

        let tracer_payload = rx.recv().await;

        assert!(tracer_payload.is_some());

        let expected_tracer_payload = pb::TracerPayload {
            container_id: "33".to_string(),
            language_name: "nodejs".to_string(),
            language_version: "v19.7.0".to_string(),
            tracer_version: "4.0.0".to_string(),
            runtime_id: "afjksdljfkllksdj-28934889".to_string(),
            chunks: vec![pb::TraceChunk {
                priority: i8::MIN as i32,
                origin: "".to_string(),
                spans: vec![
                    create_test_span(start, 333, 222, false),
                    create_test_span(start, 222, 0, true),
                    create_test_span(start, 444, 333, false),
                ],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: HashMap::new(),
            env: "test-env".to_string(),
            hostname: "".to_string(),
            app_version: "".to_string(),
        };

        assert_eq!(expected_tracer_payload, tracer_payload.unwrap());
    }
}
