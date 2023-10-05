// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use anyhow::Context;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::http::HeaderValue;
use hyper::{body::Buf, Body, Client, HeaderMap, Method, Response, StatusCode};
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use datadog_trace_normalization::normalizer;
use datadog_trace_protobuf::pb;
use datadog_trace_protobuf::pb::TraceChunk;
use ddcommon::{connector, Endpoint, HttpRequestBuilder};

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";

macro_rules! parse_string_header {
    (
        $header_map:ident,
        { $($header_key:literal => $($field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(header_value) = $header_map.get($header_key) {
                if let Ok(h) = header_value.to_str() {
                    $($field).+ = h;
                }
            }
        )+
    }
}

/// First value of returned tuple is the payload size
pub async fn get_traces_from_request_body(
    body: Body,
) -> anyhow::Result<(usize, Vec<Vec<pb::Span>>)> {
    let buffer = hyper::body::aggregate(body).await?;
    let size = buffer.remaining();

    let traces: Vec<Vec<pb::Span>> = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("Error deserializing trace from request body: {err}")
        }
    };

    if traces.is_empty() {
        anyhow::bail!("No traces deserialized from the request body.")
    }

    Ok((size, traces))
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct TracerHeaderTags<'a> {
    pub lang: &'a str,
    pub lang_version: &'a str,
    pub lang_interpreter: &'a str,
    pub lang_vendor: &'a str,
    pub tracer_version: &'a str,
    pub container_id: &'a str,
    // specifies that the client has marked top-level spans, when set. Any non-empty value will mean 'yes'.
    pub client_computed_top_level: bool,
    // specifies whether the client has computed stats so that the agent doesn't have to. Any non-empty value will mean 'yes'.
    pub client_computed_stats: bool,
}

impl<'a> From<TracerHeaderTags<'a>> for HashMap<&'static str, String> {
    fn from(tags: TracerHeaderTags<'a>) -> HashMap<&'static str, String> {
        let mut headers = HashMap::from([
            ("datadog-meta-lang", tags.lang.to_string()),
            ("datadog-meta-lang-version", tags.lang_version.to_string()),
            (
                "datadog-meta-lang-interpreter",
                tags.lang_interpreter.to_string(),
            ),
            ("datadog-meta-lang-vendor", tags.lang_vendor.to_string()),
            (
                "datadog-meta-tracer-version",
                tags.tracer_version.to_string(),
            ),
            ("datadog-container-id", tags.container_id.to_string()),
        ]);
        headers.retain(|_, v| !v.is_empty());
        headers
    }
}

impl<'a> From<&'a HeaderMap<HeaderValue>> for TracerHeaderTags<'a> {
    fn from(headers: &'a HeaderMap<HeaderValue>) -> Self {
        let mut tags = TracerHeaderTags::default();
        parse_string_header!(
            headers,
            {
                "datadog-meta-lang" => tags.lang,
                "datadog-meta-lang-version" => tags.lang_version,
                "datadog-meta-lang-interpreter" => tags.lang_interpreter,
                "datadog-meta-lang-vendor" => tags.lang_vendor,
                "datadog-meta-tracer-version" => tags.tracer_version,
                "datadog-container-id" => tags.container_id,
            }
        );
        if headers.get("datadog-client-computed-top-level").is_some() {
            tags.client_computed_top_level = true;
        }
        if headers.get("datadog-client-computed-stats").is_some() {
            tags.client_computed_stats = true;
        }
        tags
    }
}

// Tags gathered from a trace's root span
#[derive(Default)]
pub struct RootSpanTags<'a> {
    pub env: &'a str,
    pub app_version: &'a str,
    pub hostname: &'a str,
    pub runtime_id: &'a str,
}

pub fn construct_agent_payload(tracer_payloads: Vec<pb::TracerPayload>) -> pb::AgentPayload {
    pb::AgentPayload {
        host_name: "".to_string(),
        env: "".to_string(),
        agent_version: "".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
        rare_sampler_enabled: false,
    }
}

pub fn construct_trace_chunk(trace: Vec<pb::Span>) -> pb::TraceChunk {
    pb::TraceChunk {
        priority: normalizer::SamplerPriority::None as i32,
        origin: "".to_string(),
        spans: trace,
        tags: HashMap::new(),
        dropped_trace: false,
    }
}

pub fn construct_tracer_payload(
    chunks: Vec<pb::TraceChunk>,
    tracer_tags: &TracerHeaderTags,
    root_span_tags: RootSpanTags,
) -> pb::TracerPayload {
    pb::TracerPayload {
        app_version: root_span_tags.app_version.to_string(),
        language_name: tracer_tags.lang.to_string(),
        container_id: tracer_tags.container_id.to_string(),
        env: root_span_tags.env.to_string(),
        runtime_id: root_span_tags.runtime_id.to_string(),
        chunks,
        hostname: root_span_tags.hostname.to_string(),
        language_version: tracer_tags.lang_version.to_string(),
        tags: HashMap::new(),
        tracer_version: tracer_tags.tracer_version.to_string(),
    }
}

pub fn serialize_proto_payload<T>(payload: &T) -> anyhow::Result<Vec<u8>>
where
    T: prost::Message,
{
    let mut buf = Vec::new();
    buf.reserve(payload.encoded_len());
    payload.encode(&mut buf)?;
    Ok(buf)
}

#[derive(Debug, Clone)]
pub struct SendData {
    tracer_payloads: Vec<pb::TracerPayload>,
    size: usize, // have a rough size estimate to force flushing if it's large
    pub target: Endpoint,
    headers: HashMap<&'static str, String>,
}

impl SendData {
    pub fn new(
        size: usize,
        tracer_payload: pb::TracerPayload,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendData {
        let headers = if let Some(api_key) = &target.api_key {
            HashMap::from([("DD-API-KEY", api_key.as_ref().to_string())])
        } else {
            tracer_header_tags.into()
        };

        SendData {
            tracer_payloads: vec![tracer_payload],
            size,
            target: target.clone(),
            headers,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub async fn send<'a>(self) -> anyhow::Result<Response<Body>> {
        let target = &self.target;

        let mut req = hyper::Request::builder()
            .uri(target.url.clone())
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST);

        for (key, value) in &self.headers {
            req = req.header(*key, value);
        }

        async fn send_request(
            req: HttpRequestBuilder,
            payload: Vec<u8>,
            expected_status: StatusCode,
        ) -> anyhow::Result<Response<Body>> {
            let req = req.body(Body::from(payload))?;

            match Client::builder()
                .build(connector::Connector::default())
                .request(req)
                .await
            {
                Ok(response) => {
                    if response.status() != expected_status {
                        let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                        let response_body =
                            String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                        anyhow::bail!("Server did not accept traces: {response_body}");
                    }
                    Ok(response)
                }
                Err(e) => anyhow::bail!("Failed to send traces: {e}"),
            }
        }

        if target.api_key.is_some() {
            req = req.header("Content-type", "application/x-protobuf");

            let agent_payload = construct_agent_payload(self.tracer_payloads);
            let serialized_trace_payload = serialize_proto_payload(&agent_payload)
                .context("Failed to serialize trace agent payload, dropping traces")?;

            send_request(req, serialized_trace_payload, StatusCode::ACCEPTED).await
        } else {
            req = req.header("Content-type", "application/msgpack");

            let (template, _) = req.body(()).unwrap().into_parts();

            let mut futures = FuturesUnordered::new();
            for tracer_payload in self.tracer_payloads.into_iter() {
                let mut builder = HttpRequestBuilder::new()
                    .method(template.method.clone())
                    .uri(template.uri.clone())
                    .version(template.version)
                    .header(
                        "X-Datadog-Trace-Count",
                        tracer_payload.chunks.len().to_string(),
                    );
                builder
                    .headers_mut()
                    .unwrap()
                    .extend(template.headers.clone());

                futures.push(send_request(
                    builder,
                    rmp_serde::to_vec_named(&tracer_payload)?,
                    StatusCode::OK,
                ));
            }
            let mut last_response = Err(anyhow::format_err!("No futures completed...?!"));
            loop {
                match futures.next().await {
                    Some(response) => match response {
                        Ok(response) => last_response = Ok(response),
                        Err(e) => return Err(e),
                    },
                    None => return last_response,
                }
            }
        }
    }

    // For testing
    pub fn get_payloads(&self) -> &Vec<pb::TracerPayload> {
        &self.tracer_payloads
    }
}

pub fn coalesce_send_data(mut data: Vec<SendData>) -> Vec<SendData> {
    // TODO trace payloads with identical data except for chunk could be merged?

    data.sort_unstable_by(|a, b| a.target.url.to_string().cmp(&b.target.url.to_string()));
    data.dedup_by(|a, b| {
        if a.target.url == b.target.url {
            a.tracer_payloads.append(&mut b.tracer_payloads);
            a.size += b.size;
            return true;
        }
        false
    });
    data
}

pub fn get_root_span_index(trace: &Vec<pb::Span>) -> anyhow::Result<usize> {
    if trace.is_empty() {
        anyhow::bail!("Cannot find root span index in an empty trace.");
    }

    // parent_id -> (child_span, index_of_child_span_in_trace)
    let mut parent_id_to_child_map: HashMap<u64, (&pb::Span, usize)> = HashMap::new();

    // look for the span with parent_id == 0 (starting from the end) since some clients put the root span last.
    for i in (0..trace.len()).rev() {
        let cur_span = &trace[i];
        if cur_span.parent_id == 0 {
            return Ok(i);
        }
        parent_id_to_child_map.insert(cur_span.parent_id, (cur_span, i));
    }

    for span in trace {
        if parent_id_to_child_map.contains_key(&span.span_id) {
            parent_id_to_child_map.remove(&span.span_id);
        }
    }

    // if the trace is valid, parent_id_to_child_map should just have 1 entry at this point.
    if parent_id_to_child_map.len() != 1 {
        error!(
            "Could not find the root span for trace with trace_id: {}",
            &trace[0].trace_id,
        );
    }

    // pick a span without a parent
    let span_tuple = match parent_id_to_child_map.values().copied().next() {
        Some(res) => res,
        None => {
            // just return the index of the last span in the trace.
            info!("Returning index of last span in trace as root span index.");
            return Ok(trace.len() - 1);
        }
    };

    Ok(span_tuple.1)
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span(trace: &mut [pb::Span]) {
    let mut span_id_to_service: HashMap<u64, String> = HashMap::new();
    for span in trace.iter() {
        span_id_to_service.insert(span.span_id, span.service.clone());
    }
    for span in trace.iter_mut() {
        if span.parent_id == 0 {
            set_top_level_span(span, true);
            continue;
        }
        match span_id_to_service.get(&span.parent_id) {
            Some(parent_span_service) => {
                if !parent_span_service.eq(&span.service) {
                    // parent is not in the same service
                    set_top_level_span(span, true)
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(span, true)
            }
        }
    }
}

fn set_top_level_span(span: &mut pb::Span, is_top_level: bool) {
    if !is_top_level {
        if span.metrics.contains_key(TOP_LEVEL_KEY) {
            span.metrics.remove(TOP_LEVEL_KEY);
        }
        return;
    }
    span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
}

pub fn set_serverless_root_span_tags(
    span: &mut pb::Span,
    function_name: Option<String>,
    env_type: &EnvironmentType,
) {
    span.r#type = "serverless".to_string();
    let origin_tag = match env_type {
        EnvironmentType::CloudFunction => "cloudfunction",
        EnvironmentType::AzureFunction => "azurefunction",
    };
    span.meta
        .insert("_dd.origin".to_string(), origin_tag.to_string());
    span.meta
        .insert("origin".to_string(), origin_tag.to_string());

    if let Some(function_name) = function_name {
        span.meta.insert("functionname".to_string(), function_name);
    }
}

pub fn update_tracer_top_level(span: &mut pb::Span) {
    if span.metrics.contains_key(TRACER_TOP_LEVEL_KEY) {
        span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentType {
    CloudFunction,
    AzureFunction,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct MiniAgentMetadata {
    pub gcp_project_id: Option<String>,
    pub gcp_region: Option<String>,
}

pub fn enrich_span_with_mini_agent_metadata(
    span: &mut pb::Span,
    mini_agent_metadata: &MiniAgentMetadata,
) {
    if let Some(gcp_project_id) = &mini_agent_metadata.gcp_project_id {
        span.meta
            .insert("project_id".to_string(), gcp_project_id.to_string());
    }
    if let Some(gcp_region) = &mini_agent_metadata.gcp_region {
        span.meta
            .insert("location".to_string(), gcp_region.to_string());
    }
}

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

pub fn collect_trace_chunks(
    mut traces: Vec<Vec<pb::Span>>,
    tracer_header_tags: &TracerHeaderTags,
    process_chunk: impl Fn(&mut TraceChunk, usize),
) -> pb::TracerPayload {
    let mut trace_chunks: Vec<pb::TraceChunk> = Vec::new();

    let mut gathered_root_span_tags = false;
    let mut root_span_tags = RootSpanTags::default();

    for trace in traces.iter_mut() {
        if let Err(e) = normalizer::normalize_trace(trace) {
            error!("Error normalizing trace: {e}");
        }

        let mut chunk = construct_trace_chunk(trace.to_vec());

        let root_span_index = match get_root_span_index(trace) {
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
                update_tracer_top_level(span);
            }
        }

        if !tracer_header_tags.client_computed_top_level {
            compute_top_level_span(&mut chunk.spans);
        }

        process_chunk(&mut chunk, root_span_index);

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

    construct_tracer_payload(trace_chunks, tracer_header_tags, root_span_tags)
}

#[cfg(test)]
mod tests {
    use hyper::Request;
    use serde_json::json;
    use std::collections::HashMap;

    use super::{get_root_span_index, set_serverless_root_span_tags};
    use crate::{trace_test_utils::create_test_span, trace_utils};
    use datadog_trace_protobuf::pb;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_get_traces_from_request_body() {
        let pairs = vec![
            (
                json!([{
                    "service": "test-service",
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "parent_id": 333,
                    "start": 1,
                    "duration": 5,
                    "error": 0,
                    "meta": {},
                    "metrics": {},
                }]),
                vec![vec![pb::Span {
                    service: "test-service".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 333,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "".to_string(),
                }]],
            ),
            (
                json!([{
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "start": 1,
                    "duration": 5,
                    "meta": {},
                }]),
                vec![vec![pb::Span {
                    service: "".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 0,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "".to_string(),
                }]],
            ),
        ];

        for (trace_input, output) in pairs {
            let bytes = rmp_serde::to_vec(&vec![&trace_input]).unwrap();
            let request = Request::builder()
                .body(hyper::body::Body::from(bytes))
                .unwrap();
            let res = trace_utils::get_traces_from_request_body(request.into_body()).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap().1, output);
        }
    }

    #[test]
    fn test_get_root_span_index_from_complete_trace() {
        let trace = vec![
            create_test_span(1234, 12341, 0, 1, false),
            create_test_span(1234, 12342, 12341, 1, false),
            create_test_span(1234, 12343, 12342, 1, false),
        ];

        let root_span_index = get_root_span_index(&trace);
        assert!(root_span_index.is_ok());
        assert_eq!(root_span_index.unwrap(), 0);
    }

    #[test]
    fn test_get_root_span_index_from_partial_trace() {
        let trace = vec![
            create_test_span(1234, 12342, 12341, 1, false),
            create_test_span(1234, 12341, 12340, 1, false), // this is the root span, it's parent is not in the trace
            create_test_span(1234, 12343, 12342, 1, false),
        ];

        let root_span_index = get_root_span_index(&trace);
        assert!(root_span_index.is_ok());
        assert_eq!(root_span_index.unwrap(), 1);
    }

    #[test]
    fn test_set_serverless_root_span_tags_azure_function() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        set_serverless_root_span_tags(
            &mut span,
            Some("test_function".to_string()),
            &trace_utils::EnvironmentType::AzureFunction,
        );
        assert_eq!(
            span.meta,
            HashMap::from([
                (
                    "runtime-id".to_string(),
                    "test-runtime-id-value".to_string()
                ),
                ("_dd.origin".to_string(), "azurefunction".to_string()),
                ("origin".to_string(), "azurefunction".to_string()),
                ("functionname".to_string(), "test_function".to_string()),
                ("env".to_string(), "test-env".to_string()),
                ("service".to_string(), "test-service".to_string())
            ]),
        );
        assert_eq!(span.r#type, "serverless".to_string())
    }

    #[test]
    fn test_set_serverless_root_span_tags_cloud_function() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        set_serverless_root_span_tags(
            &mut span,
            Some("test_function".to_string()),
            &trace_utils::EnvironmentType::CloudFunction,
        );
        assert_eq!(
            span.meta,
            HashMap::from([
                (
                    "runtime-id".to_string(),
                    "test-runtime-id-value".to_string()
                ),
                ("_dd.origin".to_string(), "cloudfunction".to_string()),
                ("origin".to_string(), "cloudfunction".to_string()),
                ("functionname".to_string(), "test_function".to_string()),
                ("env".to_string(), "test-env".to_string()),
                ("service".to_string(), "test-service".to_string())
            ]),
        );
        assert_eq!(span.r#type, "serverless".to_string())
    }
}
