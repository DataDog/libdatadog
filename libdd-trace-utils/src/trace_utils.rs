// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "send")]
pub use crate::send_data::SendData;
#[cfg(feature = "send")]
pub use crate::send_data::send_data_result::SendDataResult;
use crate::span::v05::dict::SharedDict;
use crate::span::{TraceData, v05};
pub use crate::tracer_header_tags::{TracerGenericTags, TracerHeaderTags};
use crate::tracer_payload::TracerPayloadCollection;
use crate::tracer_payload::{self, TraceChunks};
use anyhow::anyhow;
use bytes::Buf;
use bytes::buf::Reader;
use http_body_util::BodyExt;
use libdd_host_identity::azure_app_services;
use libdd_trace_normalization::normalizer;
use libdd_trace_protobuf::pb;
use rmp::decode::read_array_len;
use rmpv::decode::read_value;
use rmpv::{Integer, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
#[cfg(feature = "send")]
use std::collections::hash_map::RandomState;
use std::env;
use tracing::error;

/// The maximum payload size for a single request that can be sent to the trace agent. Payloads
/// larger than this size will be dropped and the agent will return a 413 error if
/// `datadog-send-real-http-status` is set.
pub const MAX_PAYLOAD_SIZE: usize = 25 * 1024 * 1024;
/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";
const MAX_STRING_DICT_SIZE: u32 = 25_000_000;
const SPAN_ELEMENT_COUNT: usize = 12;

/// First value of returned tuple is the payload size
pub async fn get_traces_from_request_body<B>(body: B) -> anyhow::Result<(usize, Vec<Vec<pb::Span>>)>
where
    B: http_body::Body,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let buffer = body.collect().await?.aggregate();
    let size = buffer.remaining();

    let traces: Vec<Vec<pb::Span>> = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("Error deserializing trace from request body: {err}")
        }
    };

    Ok((size, traces))
}

#[inline]
fn get_v05_strings_dict(reader: &mut Reader<impl Buf>) -> anyhow::Result<Vec<String>> {
    let dict_size =
        read_array_len(reader).map_err(|err| anyhow!("Error reading dict size: {err}"))?;
    if dict_size > MAX_STRING_DICT_SIZE {
        anyhow::bail!(
            "Error deserializing strings dictionary. Dict size is too large: {dict_size}"
        );
    }
    let mut dict: Vec<String> = Vec::with_capacity(dict_size.try_into()?);
    for _ in 0..dict_size {
        match read_value(reader)? {
            Value::String(s) => {
                let parsed_string = s
                    .into_str()
                    .ok_or_else(|| anyhow!("Error reading string dict"))?;
                dict.push(parsed_string);
            }
            val => anyhow::bail!(
                "Error deserializing strings dictionary. Value in string dict is not a string: {val}"
            ),
        }
    }
    Ok(dict)
}

#[inline]
fn get_v05_span(reader: &mut Reader<impl Buf>, dict: &[String]) -> anyhow::Result<pb::Span> {
    let mut span: pb::Span = Default::default();
    let span_size = rmp::decode::read_array_len(reader)
        .map_err(|err| anyhow!("Error reading span size: {err}"))? as usize;
    if span_size != SPAN_ELEMENT_COUNT {
        anyhow::bail!("Expected an array of exactly 12 elements in a span, got {span_size}");
    }
    // 0 - service
    span.service = get_v05_string(reader, dict, "service")?;
    // 1 - name
    span.name = get_v05_string(reader, dict, "name")?;
    // 2 - resource
    span.resource = get_v05_string(reader, dict, "resource")?;

    // 3 - trace_id
    match read_value(reader)? {
        Value::Integer(i) => {
            span.trace_id = i.as_u64().ok_or_else(|| {
                anyhow!("Error reading span trace_id, value is not an integer: {i}")
            })?;
        }
        val => anyhow::bail!("Error reading span trace_id, value is not an integer: {val}"),
    };
    // 4 - span_id
    match read_value(reader)? {
        Value::Integer(i) => {
            span.span_id = i.as_u64().ok_or_else(|| {
                anyhow!("Error reading span span_id, value is not an integer: {i}")
            })?;
        }
        val => anyhow::bail!("Error reading span span_id, value is not an integer: {val}"),
    };
    // 5 - parent_id
    match read_value(reader)? {
        Value::Integer(i) => {
            span.parent_id = i.as_u64().ok_or_else(|| {
                anyhow!("Error reading span parent_id, value is not an integer: {i}")
            })?;
        }
        val => anyhow::bail!("Error reading span parent_id, value is not an integer: {val}"),
    };
    // 6 - start
    match read_value(reader)? {
        Value::Integer(i) => {
            span.start = i
                .as_i64()
                .ok_or_else(|| anyhow!("Error reading span start, value is not an integer: {i}"))?;
        }
        val => anyhow::bail!("Error reading span start, value is not an integer: {val}"),
    };
    // 7 - duration
    match read_value(reader)? {
        Value::Integer(i) => {
            span.duration = i.as_i64().ok_or_else(|| {
                anyhow!("Error reading span duration, value is not an integer: {i}")
            })?;
        }
        val => anyhow::bail!("Error reading span duration, value is not an integer: {val}"),
    };
    // 8 - error
    match read_value(reader)? {
        Value::Integer(i) => {
            span.error = i
                .as_i64()
                .ok_or_else(|| anyhow!("Error reading span error, value is not an integer: {i}"))?
                as i32;
        }
        val => anyhow::bail!("Error reading span error, value is not an integer: {val}"),
    }
    // 9 - meta
    match read_value(reader)? {
        Value::Map(meta) => {
            for (k, v) in meta.iter() {
                match k {
                    Value::Integer(k) => match v {
                        Value::Integer(v) => {
                            let key = str_from_dict(dict, *k)?;
                            let val = str_from_dict(dict, *v)?;
                            span.meta.insert(key, val);
                        }
                        _ => anyhow::bail!(
                            "Error reading span meta, value is not an integer and can't be looked up in dict: {v}"
                        ),
                    },
                    _ => anyhow::bail!(
                        "Error reading span meta, key is not an integer and can't be looked up in dict: {k}"
                    ),
                }
            }
        }
        val => anyhow::bail!("Error reading span meta, value is not a map: {val}"),
    }
    // 10 - metrics
    match read_value(reader)? {
        Value::Map(metrics) => {
            for (k, v) in metrics.iter() {
                match k {
                    Value::Integer(k) => {
                        match v {
                            Value::Integer(v) => {
                                let key = str_from_dict(dict, *k)?;
                                span.metrics.insert(key, v.as_f64().ok_or_else(||anyhow!("Error reading span metrics, value is not an integer: {v}"))?);
                            }
                            Value::F64(v) => {
                                let key = str_from_dict(dict, *k)?;
                                span.metrics.insert(key, *v);
                            }
                            _ => anyhow::bail!(
                                "Error reading span metrics, value is not a float or integer: {v}"
                            ),
                        }
                    }
                    _ => anyhow::bail!("Error reading span metrics, key is not an integer: {k}"),
                }
            }
        }
        val => anyhow::bail!("Error reading span metrics, value is not a map: {val}"),
    }

    // 11 - type
    match read_value(reader)? {
        Value::Integer(s) => span.r#type = str_from_dict(dict, s)?,
        val => anyhow::bail!("Error reading span type, value is not an integer: {val}"),
    }
    Ok(span)
}

#[inline]
fn str_from_dict(dict: &[String], id: Integer) -> anyhow::Result<String> {
    let id = id
        .as_i64()
        .ok_or_else(|| anyhow!("Error reading string from dict, id is not an integer: {id}"))?
        as usize;
    if id >= dict.len() {
        anyhow::bail!("Error reading string from dict, id out of bounds: {id}");
    }
    Ok(dict[id].to_string())
}

#[inline]
fn get_v05_string(
    reader: &mut Reader<impl Buf>,
    dict: &[String],
    field_name: &str,
) -> anyhow::Result<String> {
    match read_value(reader)? {
        Value::Integer(s) => str_from_dict(dict, s),
        val => anyhow::bail!(
            "Error reading {field_name}, value is not an integer and can't be looked up in dict: {val}"
        ),
    }
}

pub async fn get_v05_traces_from_request_body<B>(
    body: B,
) -> anyhow::Result<(usize, Vec<Vec<pb::Span>>)>
where
    B: http_body::Body,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let buffer = body.collect().await?.aggregate();
    let body_size = buffer.remaining();
    let mut reader = buffer.reader();
    let wrapper_size = read_array_len(&mut reader)?;
    if wrapper_size != 2 {
        anyhow::bail!("Expected an arrary of exactly 2 elements, got {wrapper_size}");
    }

    let dict = get_v05_strings_dict(&mut reader)?;

    let traces_size = rmp::decode::read_array_len(&mut reader)?;
    let mut traces: Vec<Vec<pb::Span>> = Default::default();

    for _ in 0..traces_size {
        let spans_size = rmp::decode::read_array_len(&mut reader)?;
        let mut trace: Vec<pb::Span> = Default::default();

        for _ in 0..spans_size {
            let span = get_v05_span(&mut reader, &dict)?;
            trace.push(span);
        }
        traces.push(trace);
    }
    Ok((body_size, traces))
}

/// Tags extracted from a tracer payload's traces, used to populate top level tracer payload fields.
#[derive(Default)]
pub struct TracerPayloadTags {
    pub env: String,
    pub app_version: String,
    pub hostname: String,
    pub runtime_id: String,
}

/// Returns the first non-empty value of `field` found in `trace`, searching the root span first
/// then all other spans.
fn search_trace_for_field(root: &pb::Span, trace: &[pb::Span], field: &str) -> Option<String> {
    if let Some(v) = root.meta.get(field)
        && !v.is_empty()
    {
        return Some(v.clone());
    }
    for span in trace {
        if span.span_id == root.span_id {
            continue;
        }
        if let Some(v) = span.meta.get(field)
            && !v.is_empty()
        {
            return Some(v.clone());
        }
    }
    None
}

pub(crate) fn construct_trace_chunk(trace: Vec<pb::Span>) -> pb::TraceChunk {
    pb::TraceChunk {
        priority: normalizer::SamplerPriority::None as i32,
        origin: "".to_string(),
        spans: trace,
        tags: HashMap::new(),
        dropped_trace: false,
    }
}

pub(crate) fn construct_tracer_payload(
    chunks: Vec<pb::TraceChunk>,
    tracer_tags: &TracerHeaderTags,
    tracer_payload_tags: TracerPayloadTags,
) -> pb::TracerPayload {
    pb::TracerPayload {
        app_version: tracer_payload_tags.app_version,
        language_name: tracer_tags.lang.to_string(),
        container_id: tracer_tags.container_id.to_string(),
        env: tracer_payload_tags.env,
        runtime_id: tracer_payload_tags.runtime_id,
        chunks,
        hostname: tracer_payload_tags.hostname,
        language_version: tracer_tags.lang_version.to_string(),
        tags: HashMap::new(),
        tracer_version: tracer_tags.tracer_version.to_string(),
        container_debug: None,
    }
}

pub(crate) fn cmp_send_data_payloads(a: &pb::TracerPayload, b: &pb::TracerPayload) -> Ordering {
    a.tracer_version
        .cmp(&b.tracer_version)
        .then(a.language_version.cmp(&b.language_version))
        .then(a.language_name.cmp(&b.language_name))
        .then(a.hostname.cmp(&b.hostname))
        .then(a.container_id.cmp(&b.container_id))
        .then(a.runtime_id.cmp(&b.runtime_id))
        .then(a.env.cmp(&b.env))
        .then(a.app_version.cmp(&b.app_version))
        .then(a.container_debug.cmp(&b.container_debug))
}

#[cfg(feature = "send")]
pub fn coalesce_send_data(mut data: Vec<SendData>) -> Vec<SendData> {
    let hashers = [RandomState::new(), RandomState::new()];
    data.sort_by_cached_key(|data| {
        (
            data.get_target().url.to_string(),
            data.get_target().test_token.clone(),
            data.tracer_payloads.v1_group_key(&hashers),
        )
    });
    data.dedup_by(|a, b| {
        if a.get_target().url == b.get_target().url
            && a.get_target().test_token == b.get_target().test_token
        {
            // Size is only an approximation. In practice it won't vary much, but be safe here.
            // We also don't care about the exact maximum size, like two 25 MB or one 50 MB request
            // has similar results. The primary goal here is avoiding many small requests.
            // TODO: maybe make the MAX_PAYLOAD_SIZE configurable?
            if a.size + b.size < MAX_PAYLOAD_SIZE / 2 {
                // Note: dedup_by drops a, and retains b. Only drop a if the append actually
                // merged its data into b; otherwise keep both entries (e.g. diverging V1
                // tracer metadata) so a's traces aren't silently lost.
                if b.tracer_payloads.append(&mut a.tracer_payloads) {
                    b.size += a.size;
                    return true;
                }
            }
        }
        false
    });
    // Merge chunks with common properties. Reduces requests for agentful mode.
    // And reduces a little bit of data for agentless.
    for send_data in data.iter_mut() {
        send_data.tracer_payloads.merge();
    }
    data
}

/// Returns the index of the root span in `trace`, see
/// [`crate::span::trace_utils::get_root_span_index`].
pub fn get_root_span_index(trace: &[pb::Span]) -> anyhow::Result<usize> {
    crate::span::trace_utils::get_root_span_index(trace)
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
            set_top_level_span(span);
            continue;
        }
        match span_id_to_service.get(&span.parent_id) {
            Some(parent_span_service) => {
                if !parent_span_service.eq(&span.service) {
                    // parent is not in the same service
                    set_top_level_span(span)
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(span)
            }
        }
    }
}

/// Return true if the span has a top level key set
pub fn has_top_level(span: &pb::Span) -> bool {
    span.metrics
        .get(TRACER_TOP_LEVEL_KEY)
        .is_some_and(|v| *v == 1.0)
        || span.metrics.get(TOP_LEVEL_KEY).is_some_and(|v| *v == 1.0)
}

fn set_top_level_span(span: &mut pb::Span) {
    span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
}

pub fn set_serverless_root_span_tags(
    span: &mut pb::Span,
    app_name: Option<String>,
    env_type: &EnvironmentType,
) {
    let origin_tag = match env_type {
        EnvironmentType::CloudFunction => "cloudfunction",
        EnvironmentType::AzureFunction => "azurefunction",
        EnvironmentType::AzureSpringApp => "azurespringapp",
        EnvironmentType::LambdaFunction => "lambda", // historical reasons
    };
    span.meta
        .insert("_dd.origin".to_string(), origin_tag.to_string());
    span.meta
        .insert("origin".to_string(), origin_tag.to_string());

    if let Some(function_name) = app_name {
        match env_type {
            EnvironmentType::CloudFunction
            | EnvironmentType::AzureFunction
            | EnvironmentType::LambdaFunction => {
                span.meta.insert("functionname".to_string(), function_name);
            }
            _ => {}
        }
    }
}

fn update_tracer_top_level(span: &mut pb::Span) {
    if span.metrics.contains_key(TRACER_TOP_LEVEL_KEY) {
        span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentType {
    CloudFunction,
    AzureFunction,
    AzureSpringApp,
    LambdaFunction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiniAgentMetadata {
    pub azure_spring_app_hostname: Option<String>,
    pub azure_spring_app_name: Option<String>,
    pub gcp_project_id: Option<String>,
    pub gcp_region: Option<String>,
    pub version: Option<String>,
}

impl Default for MiniAgentMetadata {
    fn default() -> Self {
        MiniAgentMetadata {
            azure_spring_app_hostname: Default::default(),
            azure_spring_app_name: Default::default(),
            gcp_project_id: Default::default(),
            gcp_region: Default::default(),
            version: env::var("DD_SERVERLESS_COMPAT_VERSION").ok(),
        }
    }
}

pub fn enrich_span_with_mini_agent_metadata(
    span: &mut pb::Span,
    mini_agent_metadata: &MiniAgentMetadata,
) {
    if let Some(azure_spring_app_hostname) = &mini_agent_metadata.azure_spring_app_hostname {
        span.meta.insert(
            "asa.hostname".to_string(),
            azure_spring_app_hostname.to_string(),
        );
    }
    if let Some(azure_spring_app_name) = &mini_agent_metadata.azure_spring_app_name {
        span.meta
            .insert("asa.name".to_string(), azure_spring_app_name.to_string());
    }
    if let Some(serverless_compat_version) = &mini_agent_metadata.version {
        span.meta.insert(
            "_dd.serverless_compat_version".to_string(),
            serverless_compat_version.to_string(),
        );
    }
}

pub fn enrich_span_with_google_cloud_function_metadata(
    span: &mut pb::Span,
    mini_agent_metadata: &MiniAgentMetadata,
    function: Option<String>,
) {
    #[allow(clippy::todo)]
    let Some(region) = &mini_agent_metadata.gcp_region else {
        todo!()
    };
    #[allow(clippy::todo)]
    let Some(project) = &mini_agent_metadata.gcp_project_id else {
        todo!()
    };

    if let Some(function) = function
        && !region.is_empty()
        && !project.is_empty()
    {
        let resource_name = format!(
            "projects/{}/locations/{}/functions/{}",
            project, region, function
        );

        span.meta
            .insert("gcrfx.location".to_string(), region.to_string());
        span.meta
            .insert("gcrfx.project_id".to_string(), project.to_string());
        span.meta
            .insert("gcrfx.resource_name".to_string(), resource_name.to_string());
    }
}

pub fn enrich_span_with_azure_function_metadata(span: &mut pb::Span) {
    if span.name == "azure.apim" {
        return;
    }

    if let Some(aas_metadata) = &*azure_app_services::AAS_METADATA_FUNCTION {
        span.meta.extend(
            aas_metadata
                .get_function_tags()
                .map(|(name, value)| (name.to_string(), value.to_string())),
        );
    }
}

/// Converts v0.4-shaped span chunks into the v0.5 wire representation.
///
/// v0.5 deduplicates every string field across the whole payload through a shared dictionary
/// and replaces them with `u32` indices. This walks each span via [`v05::from_v04_span`],
/// interning strings into the [`SharedDict`] as it goes, and returns the resulting
/// `(dict, traces)` pair wrapped in [`TraceChunks::V05`].
///
/// Returns `Err` if any span fails to convert (e.g. unsupported field value); the partial
/// dictionary built so far is discarded.
pub fn convert_trace_chunks_v04_to_v05<T: TraceData>(
    traces: &[Vec<crate::span::v04::Span<T>>],
) -> anyhow::Result<TraceChunks<T>> {
    let mut shared_dict = SharedDict::default();
    let mut v05_traces: Vec<Vec<v05::Span>> = Vec::with_capacity(traces.len());
    for trace in traces {
        let v05_trace = trace
            .iter()
            .map(|span| v05::from_v04_span(span, &mut shared_dict))
            .collect::<anyhow::Result<Vec<_>>>()?;
        v05_traces.push(v05_trace);
    }
    Ok(TraceChunks::V05((shared_dict, v05_traces)))
}

pub fn collect_pb_trace_chunks<T: tracer_payload::TraceChunkProcessor>(
    mut traces: Vec<Vec<pb::Span>>,
    tracer_header_tags: &TracerHeaderTags,
    process_chunk: &mut T,
    is_agentless: bool,
) -> anyhow::Result<TracerPayloadCollection> {
    let mut trace_chunks: Vec<pb::TraceChunk> = Vec::new();

    // We'll skip setting the global metadata and rely on the agent to unpack these
    let mut tracer_payload_tags = TracerPayloadTags::default();

    for trace in traces.iter_mut() {
        if is_agentless && let Err(e) = normalizer::normalize_trace(trace) {
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
            if tracer_header_tags.generic.client_computed_top_level {
                update_tracer_top_level(span);
            }
        }

        if !tracer_header_tags.generic.client_computed_top_level {
            compute_top_level_span(&mut chunk.spans);
        }

        process_chunk.process(&mut chunk, root_span_index);

        trace_chunks.push(chunk);

        if is_agentless {
            // Check each field independently so that a later trace can fill in fields missing
            // from an earlier trace.
            let root = &trace[root_span_index];
            if tracer_payload_tags.env.is_empty()
                && let Some(mut v) = search_trace_for_field(root, trace, "env")
            {
                // Normalize env tag in case the span it was pulled from was skipped during
                // normalization
                libdd_trace_normalization::normalize_utils::normalize_tag_value(&mut v);
                if !v.is_empty() {
                    tracer_payload_tags.env = v;
                }
            }
            if tracer_payload_tags.app_version.is_empty()
                && let Some(v) = search_trace_for_field(root, trace, "version")
            {
                tracer_payload_tags.app_version = v;
            }
            if tracer_payload_tags.hostname.is_empty()
                && let Some(v) = search_trace_for_field(root, trace, "_dd.hostname")
            {
                tracer_payload_tags.hostname = v;
            }
            if tracer_payload_tags.runtime_id.is_empty()
                && let Some(v) = search_trace_for_field(root, trace, "runtime-id")
            {
                tracer_payload_tags.runtime_id = v;
            }
        }
    }

    Ok(TracerPayloadCollection::V07(vec![
        construct_tracer_payload(trace_chunks, tracer_header_tags, tracer_payload_tags),
    ]))
}

/// Returns true if a span should be measured (i.e., it should get trace metrics calculated).
pub fn is_measured(span: &pb::Span) -> bool {
    span.metrics.get(MEASURED_KEY).is_some_and(|v| *v == 1.0)
}

/// Returns true if the span is a partial snapshot.
/// This kind of spans are partial images of long-running spans.
/// When incomplete, a partial snapshot has a metric _dd.partial_version which is a positive
/// integer. The metric usually increases each time a new version of the same span is sent by the
/// tracer
pub fn is_partial_snapshot(span: &pb::Span) -> bool {
    span.metrics
        .get(PARTIAL_VERSION_KEY)
        .is_some_and(|v| *v >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        span::{SharedDictBytes, v1},
        test_utils::{create_test_no_alloc_span, create_test_span},
    };
    use http::Request;
    use libdd_common::Endpoint;
    use libdd_networking::http_common;
    use libdd_tinybytes::BytesString;
    use serde_json::json;

    fn find_index_in_dict(dict: &SharedDictBytes, value: &str) -> Option<u32> {
        let idx = dict.iter().position(|e| e.as_str() == value);
        idx.map(|idx| idx.try_into().unwrap())
    }

    #[test]
    fn test_coalescing_does_not_exceed_max_size() {
        fn dummy() -> SendData {
            SendData::new(
                MAX_PAYLOAD_SIZE / 5 + 1,
                TracerPayloadCollection::V07(vec![pb::TracerPayload {
                    container_id: "".to_string(),
                    language_name: "".to_string(),
                    language_version: "".to_string(),
                    tracer_version: "".to_string(),
                    runtime_id: "".to_string(),
                    chunks: vec![pb::TraceChunk {
                        priority: 0,
                        origin: "".to_string(),
                        spans: vec![],
                        tags: Default::default(),
                        dropped_trace: false,
                    }],
                    tags: Default::default(),
                    env: "".to_string(),
                    hostname: "".to_string(),
                    app_version: "".to_string(),
                    container_debug: None,
                }]),
                TracerHeaderTags::default(),
                &Endpoint::default(),
            )
        }
        let coalesced = coalesce_send_data(vec![dummy(), dummy(), dummy(), dummy(), dummy()]);
        assert_eq!(
            5,
            coalesced
                .iter()
                .map(|s| s.tracer_payloads.size())
                .sum::<usize>()
        );
        // assert some chunks are actually coalesced
        assert!(
            coalesced
                .iter()
                .map(|s| {
                    if let TracerPayloadCollection::V07(collection) = &s.tracer_payloads {
                        collection.iter().map(|s| s.chunks.len()).max().unwrap()
                    } else {
                        0
                    }
                })
                .max()
                .unwrap()
                > 1
        );
        assert!(coalesced.len() > 1 && coalesced.len() < 5);
    }

    fn v1_send_data(runtime_id: &str) -> SendData {
        SendData::new(
            100,
            TracerPayloadCollection::V1(Box::new(v1::TracerPayload {
                runtime_id: BytesString::from_slice(runtime_id.as_bytes()).unwrap(),
                chunks: vec![v1::TraceChunk {
                    spans: vec![v1::Span::default()],
                    ..Default::default()
                }],
                ..Default::default()
            })),
            TracerHeaderTags::default(),
            &Endpoint::default(),
        )
    }

    fn v1_runtime_id_and_chunks(send_data: &SendData) -> (&str, usize) {
        match &send_data.tracer_payloads {
            TracerPayloadCollection::V1(payload) => {
                (payload.runtime_id.as_str(), payload.chunks.len())
            }
            _ => panic!("expected a V1 payload"),
        }
    }

    #[test]
    fn test_coalesce_v1_interleaved_runtime_ids() {
        let coalesced = coalesce_send_data(vec![
            v1_send_data("A"),
            v1_send_data("B"),
            v1_send_data("A"),
            v1_send_data("B"),
        ]);
        let mut entries: Vec<_> = coalesced.iter().map(v1_runtime_id_and_chunks).collect();
        entries.sort_unstable();
        assert_eq!(vec![("", 4)], entries);
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        let mut runtime_ids: Vec<_> = payload
            .chunks
            .iter()
            .map(|chunk| {
                chunk.spans[0]
                    .attributes
                    .get("runtime-id")
                    .unwrap()
                    .as_str()
                    .unwrap()
            })
            .collect();
        runtime_ids.sort_unstable();
        assert_eq!(runtime_ids, ["A", "A", "B", "B"]);
        assert_eq!(
            4,
            coalesced
                .iter()
                .map(|s| s.tracer_payloads.size())
                .sum::<usize>()
        );
    }

    #[test]
    fn test_coalesce_v1_single_runtime_keeps_payload_runtime_id() {
        let coalesced = coalesce_send_data(vec![v1_send_data("A"), v1_send_data("A")]);
        assert_eq!(coalesced.len(), 1);
        assert_eq!(v1_runtime_id_and_chunks(&coalesced[0]), ("A", 2));
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        for chunk in &payload.chunks {
            assert!(!chunk.spans[0].attributes.contains_key("runtime-id"));
        }
    }

    #[test]
    fn test_coalesce_v1_runtime_id_carriers() {
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            let mut send_data = v1_send_data(runtime_id);
            let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                panic!("expected a V1 payload");
            };
            payload.chunks = [
                None,
                Some(-128),
                Some(-2),
                Some(-1),
                Some(0),
                Some(1),
                Some(2),
            ]
            .into_iter()
            .map(|priority| {
                let mut spans: Vec<_> = (0..4)
                    .map(|_| v1::Span {
                        service: BytesString::from_static(runtime_id),
                        ..Default::default()
                    })
                    .collect();
                // A non-numeric sampling marker is not recognized by the agent.
                spans[0].attributes.insert(
                    BytesString::from_static("_dd.span_sampling.mechanism"),
                    v1::AttributeValue::String(BytesString::from_static("8")),
                );
                spans[1].attributes.insert(
                    BytesString::from_static("_dd.span_sampling.mechanism"),
                    v1::AttributeValue::Float(8.0),
                );
                spans[2].attributes.insert(
                    BytesString::from_static("_dd.span_sampling.mechanism"),
                    v1::AttributeValue::Int(8),
                );
                spans[3].attributes.insert(
                    BytesString::from_static("runtime-id"),
                    v1::AttributeValue::String(BytesString::from_static("explicit")),
                );
                v1::TraceChunk {
                    priority,
                    spans,
                    ..Default::default()
                }
            })
            .collect();
            data.push(send_data);
        }

        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), 1);
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        assert!(payload.runtime_id.is_empty());
        assert_eq!(payload.chunks.len(), 14);
        for chunk in &payload.chunks {
            let rejected = chunk.priority.is_some_and(|priority| priority <= 0);
            for (index, span) in chunk.spans.iter().enumerate() {
                let expected = if index == 3 {
                    Some("explicit")
                } else if index == 0 || (rejected && index == 1) {
                    Some(span.service.as_str())
                } else {
                    None
                };
                assert_eq!(
                    span.attributes
                        .get("runtime-id")
                        .and_then(v1::AttributeValue::as_str),
                    expected,
                    "priority {:?}, span {index}",
                    chunk.priority,
                );
            }
        }
    }

    #[test]
    fn test_coalesce_v1_empty_and_rejected_chunks_without_sampled_spans() {
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            let mut send_data = v1_send_data(runtime_id);
            let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                panic!("expected a V1 payload");
            };
            payload.chunks[0].priority = Some(0);
            payload.chunks.push(v1::TraceChunk::default());
            data.push(send_data);
        }
        let coalesced = coalesce_send_data(data);
        assert_eq!(v1_runtime_id_and_chunks(&coalesced[0]), ("", 4));
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        let mut runtime_ids: Vec<_> = payload
            .chunks
            .iter()
            .flat_map(|chunk| &chunk.spans)
            .map(|span| span.attributes.get("runtime-id").unwrap().as_str().unwrap())
            .collect();
        runtime_ids.sort_unstable();
        assert_eq!(runtime_ids, ["A", "B"]);
    }

    #[test]
    fn test_coalesce_v1_first_span_is_single_span_sampled() {
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            let mut send_data = v1_send_data(runtime_id);
            let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                panic!("expected a V1 payload");
            };
            let chunk = &mut payload.chunks[0];
            chunk.priority = Some(0);
            chunk.spans.push(v1::Span::default());
            for span in &mut chunk.spans {
                span.attributes.insert(
                    BytesString::from_static("_dd.span_sampling.mechanism"),
                    v1::AttributeValue::Int(8),
                );
            }
            data.push(send_data);
        }
        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), 1);
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        for chunk in &payload.chunks {
            assert!(chunk.spans[0].attributes.contains_key("runtime-id"));
            assert!(!chunk.spans[1].attributes.contains_key("runtime-id"));
        }
    }

    #[test]
    fn test_coalesce_v1_does_not_move_runtime_id_when_size_prevents_merge() {
        let mut data = vec![v1_send_data("A"), v1_send_data("B")];
        for send_data in &mut data {
            send_data.size = MAX_PAYLOAD_SIZE / 2;
        }
        let coalesced = coalesce_send_data(data);
        let mut entries: Vec<_> = coalesced.iter().map(v1_runtime_id_and_chunks).collect();
        entries.sort_unstable();
        assert_eq!(entries, [("A", 1), ("B", 1)]);
    }

    #[test]
    fn test_coalesce_v1_many_runtimes() {
        let data = (0..1000)
            .map(|index| v1_send_data(&format!("runtime-{index}")))
            .collect();
        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), 1);
        assert_eq!(v1_runtime_id_and_chunks(&coalesced[0]), ("", 1000));
        let TracerPayloadCollection::V1(payload) = &coalesced[0].tracer_payloads else {
            panic!("expected a V1 payload");
        };
        let encoded = crate::msgpack_encoder::v1::to_vec_from_v1(payload);
        let (payload, _) = crate::msgpack_decoder::v1::from_bytes(encoded.into()).unwrap();
        assert!(payload.runtime_id.is_empty());
        let runtime_ids: std::collections::HashSet<_> = payload
            .chunks
            .iter()
            .map(|chunk| {
                chunk.spans[0]
                    .attributes
                    .get("runtime-id")
                    .unwrap()
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(runtime_ids.len(), 1000);
    }

    #[test]
    fn test_coalesce_v1_diverging_attributes_not_merged() {
        fn with_attribute(runtime_id: &str, value: i64) -> SendData {
            let mut send_data = v1_send_data(runtime_id);
            if let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads {
                payload.attributes.insert(
                    BytesString::from_static("key"),
                    v1::AttributeValue::Int(value),
                );
            }
            send_data
        }
        let coalesced = coalesce_send_data(vec![with_attribute("A", 1), with_attribute("B", 2)]);
        let mut entries: Vec<_> = coalesced.iter().map(v1_runtime_id_and_chunks).collect();
        entries.sort_unstable();
        assert_eq!(vec![("A", 1), ("B", 1)], entries);
    }

    #[test]
    fn test_coalesce_v1_interleaved_attributes_and_runtime_ids() {
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            for value in [1, 2] {
                let mut send_data = v1_send_data(runtime_id);
                let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                    panic!("expected a V1 payload");
                };
                payload.attributes.insert(
                    BytesString::from_static("key"),
                    v1::AttributeValue::Int(value),
                );
                data.push(send_data);
            }
        }
        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), 2);
        for send_data in &coalesced {
            assert_eq!(v1_runtime_id_and_chunks(send_data), ("", 2));
            let TracerPayloadCollection::V1(payload) = &send_data.tracer_payloads else {
                panic!("expected a V1 payload");
            };
            let mut runtime_ids: Vec<_> = payload
                .chunks
                .iter()
                .map(|chunk| {
                    chunk.spans[0]
                        .attributes
                        .get("runtime-id")
                        .unwrap()
                        .as_str()
                        .unwrap()
                })
                .collect();
            runtime_ids.sort_unstable();
            assert_eq!(runtime_ids, ["A", "B"]);
        }
    }

    #[test]
    fn test_coalesce_v1_equivalent_attribute_maps() {
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            let mut nested = crate::span::vec_map::VecMap::new();
            if runtime_id == "A" {
                nested.insert(BytesString::from_static("z"), v1::AttributeValue::Int(-1));
                nested.insert(
                    BytesString::from_static("a"),
                    v1::AttributeValue::Float(0.0),
                );
                nested.insert(BytesString::from_static("z"), v1::AttributeValue::Int(42));
            } else {
                nested.insert(BytesString::from_static("z"), v1::AttributeValue::Int(42));
                nested.insert(
                    BytesString::from_static("a"),
                    v1::AttributeValue::Float(-0.0),
                );
            }
            let mut entries = vec![
                (
                    BytesString::from_static("nested"),
                    v1::AttributeValue::KeyValueList(nested),
                ),
                (
                    BytesString::from_static("array"),
                    v1::AttributeValue::Array(vec![
                        v1::AttributeValue::String(BytesString::from_static("text")),
                        v1::AttributeValue::Bool(true),
                        v1::AttributeValue::Int(7),
                        v1::AttributeValue::Float(3.5),
                        v1::AttributeValue::Bytes(libdd_tinybytes::Bytes::from_static(b"bytes")),
                    ]),
                ),
                (
                    BytesString::from_static("zero"),
                    v1::AttributeValue::Float(if runtime_id == "A" { 0.0 } else { -0.0 }),
                ),
            ];
            if runtime_id == "B" {
                entries.reverse();
            }
            let mut send_data = v1_send_data(runtime_id);
            let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                panic!("expected a V1 payload");
            };
            payload
                .attributes
                .insert(BytesString::from_static("zero"), v1::AttributeValue::Int(1));
            for (key, value) in entries {
                payload.attributes.insert(key, value);
            }
            data.push(send_data);
        }
        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), 1);
        assert_eq!(v1_runtime_id_and_chunks(&coalesced[0]), ("", 2));
    }

    #[test]
    fn test_coalesce_v1_distinct_attribute_types_and_array_orders() {
        let values = vec![
            v1::AttributeValue::String(BytesString::from_static("1")),
            v1::AttributeValue::Bool(true),
            v1::AttributeValue::Int(1),
            v1::AttributeValue::Float(1.0),
            v1::AttributeValue::Bytes(libdd_tinybytes::Bytes::from_static(b"1")),
            v1::AttributeValue::Array(vec![v1::AttributeValue::Int(1), v1::AttributeValue::Int(2)]),
            v1::AttributeValue::Array(vec![v1::AttributeValue::Int(2), v1::AttributeValue::Int(1)]),
            v1::AttributeValue::Array(vec![]),
            v1::AttributeValue::KeyValueList(
                vec![(BytesString::from_static("a"), v1::AttributeValue::Int(1))].into(),
            ),
            v1::AttributeValue::KeyValueList(Default::default()),
        ];
        let mut data = Vec::new();
        for runtime_id in ["A", "B"] {
            for value in &values {
                let mut send_data = v1_send_data(runtime_id);
                let TracerPayloadCollection::V1(payload) = &mut send_data.tracer_payloads else {
                    panic!("expected a V1 payload");
                };
                payload
                    .attributes
                    .insert(BytesString::from_static("key"), value.clone());
                data.push(send_data);
            }
        }
        let coalesced = coalesce_send_data(data);
        assert_eq!(coalesced.len(), values.len());
        for send_data in &coalesced {
            assert_eq!(v1_runtime_id_and_chunks(send_data), ("", 2));
        }
    }

    #[test]
    fn test_coalesce_v1_and_v04_payloads() {
        fn v04_send_data() -> SendData {
            SendData::new(
                100,
                TracerPayloadCollection::V04(vec![vec![create_test_no_alloc_span(
                    0, 1, 0, 2, true,
                )]]),
                TracerHeaderTags::default(),
                &Endpoint::default(),
            )
        }
        let coalesced = coalesce_send_data(vec![
            v04_send_data(),
            v1_send_data("A"),
            v04_send_data(),
            v1_send_data("A"),
        ]);
        assert_eq!(2, coalesced.len());
        for send_data in &coalesced {
            assert_eq!(2, send_data.tracer_payloads.size());
        }
        assert!(
            coalesced
                .iter()
                .any(|s| matches!(s.tracer_payloads, TracerPayloadCollection::V04(_)))
        );
    }

    #[tokio::test]
    #[allow(clippy::type_complexity)]
    #[cfg_attr(all(miri, target_os = "macos"), ignore)]
    async fn test_get_v05_traces_from_request_body() {
        let data: (
            Vec<String>,
            Vec<
                Vec<(
                    u8,
                    u8,
                    u8,
                    u64,
                    u64,
                    u64,
                    i64,
                    i64,
                    i32,
                    HashMap<u8, u8>,
                    HashMap<u8, f64>,
                    u8,
                )>,
            >,
        ) = (
            vec![
                "baggage".to_string(),
                "item".to_string(),
                "elasticsearch.version".to_string(),
                "7.0".to_string(),
                "my-name".to_string(),
                "X".to_string(),
                "my-service".to_string(),
                "my-resource".to_string(),
                "_dd.sampling_rate_whatever".to_string(),
                "value whatever".to_string(),
                "sql".to_string(),
            ],
            vec![vec![(
                6,
                4,
                7,
                1,
                2,
                3,
                123,
                456,
                1,
                HashMap::from([(8, 9), (0, 1), (2, 3)]),
                HashMap::from([(5, 1.2)]),
                10,
            )]],
        );
        let bytes = rmp_serde::to_vec(&data).unwrap();
        let res = get_v05_traces_from_request_body(http_common::Body::from(bytes)).await;
        assert!(res.is_ok());
        let (_, traces) = res.unwrap();
        let span = traces[0][0].clone();
        let test_span = pb::Span {
            service: "my-service".to_string(),
            name: "my-name".to_string(),
            resource: "my-resource".to_string(),
            trace_id: 1,
            span_id: 2,
            parent_id: 3,
            start: 123,
            duration: 456,
            error: 1,
            meta: HashMap::from([
                ("baggage".to_string(), "item".to_string()),
                ("elasticsearch.version".to_string(), "7.0".to_string()),
                (
                    "_dd.sampling_rate_whatever".to_string(),
                    "value whatever".to_string(),
                ),
            ]),
            metrics: HashMap::from([("X".to_string(), 1.2)]),
            meta_struct: HashMap::default(),
            r#type: "sql".to_string(),
            span_links: vec![],
            span_events: vec![],
        };
        assert_eq!(span, test_span);
    }

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
                    span_links: vec![],
                    span_events: vec![],
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
                    span_links: vec![],
                    span_events: vec![],
                }]],
            ),
        ];

        for (trace_input, output) in pairs {
            let bytes = rmp_serde::to_vec(&vec![&trace_input]).unwrap();
            let request = Request::builder()
                .body(http_common::Body::from(bytes))
                .unwrap();
            let res = get_traces_from_request_body(request.into_body()).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap().1, output);
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_get_traces_from_request_body_with_span_links() {
        let trace_input = json!([[{
            "service": "test-service",
            "name": "test-name",
            "resource": "test-resource",
            "trace_id": 111,
            "span_id": 222,
            "parent_id": 333,
            "start": 1,
            "duration": 5,
            "error": 0,
            "meta": {},
            "metrics": {},
            "span_links": [{
                "trace_id": 999,
                "span_id": 888,
                "trace_id_high": 777,
                "attributes": {"key": "value"},
                "tracestate": "vendor=value"
                // flags field intentionally omitted
            }]
        }]]);

        let expected_output = vec![vec![pb::Span {
            service: "test-service".to_string(),
            name: "test-name".to_string(),
            resource: "test-resource".to_string(),
            trace_id: 111,
            span_id: 222,
            parent_id: 333,
            start: 1,
            duration: 5,
            error: 0,
            meta: HashMap::new(),
            metrics: HashMap::new(),
            meta_struct: HashMap::new(),
            r#type: String::new(),
            span_links: vec![pb::SpanLink {
                trace_id: 999,
                span_id: 888,
                trace_id_high: 777,
                attributes: HashMap::from([("key".to_string(), "value".to_string())]),
                tracestate: "vendor=value".to_string(),
                flags: 0, // Should default to 0 when omitted
            }],
            span_events: vec![],
        }]];

        let bytes = rmp_serde::to_vec(&trace_input).unwrap();
        let request = Request::builder()
            .body(http_common::Body::from(bytes))
            .unwrap();

        let res = get_traces_from_request_body(request.into_body()).await;
        assert!(res.is_ok(), "Failed to deserialize: {res:?}");
        assert_eq!(res.unwrap().1, expected_output);
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
            create_test_span(1234, 12341, 12340, 1, false), /* this is the root span, it's
                                                             * parent is not in the trace */
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
            &EnvironmentType::AzureFunction,
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
    }

    #[test]
    fn test_set_serverless_root_span_tags_cloud_function() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        set_serverless_root_span_tags(
            &mut span,
            Some("test_function".to_string()),
            &EnvironmentType::CloudFunction,
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
    }

    #[test]
    fn test_has_top_level() {
        let top_level_span = create_test_span(123, 1234, 12, 1, true);
        let not_top_level_span = create_test_span(123, 1234, 12, 1, false);
        assert!(has_top_level(&top_level_span));
        assert!(!has_top_level(&not_top_level_span));
    }

    #[test]
    fn test_is_measured() {
        let mut measured_span = create_test_span(123, 1234, 12, 1, true);
        measured_span.metrics.insert(MEASURED_KEY.into(), 1.0);
        let not_measured_span = create_test_span(123, 1234, 12, 1, true);
        assert!(is_measured(&measured_span));
        assert!(!is_measured(&not_measured_span));
    }

    #[test]
    fn test_compute_top_level() {
        let mut span_with_different_service = create_test_span(123, 5, 2, 1, false);
        span_with_different_service.service = "another_service".into();
        let mut trace = vec![
            // Root span, should be marked as top-level
            create_test_span(123, 1, 0, 1, false),
            // Should not be marked as top-level
            create_test_span(123, 2, 1, 1, false),
            // No parent in local trace, should be marked as
            // top-level
            create_test_span(123, 4, 3, 1, false),
            // Parent belongs to another service, should be marked
            // as top-level
            span_with_different_service,
        ];

        compute_top_level_span(trace.as_mut_slice());

        let spans_marked_as_top_level: Vec<u64> = trace
            .iter()
            .filter_map(|span| {
                if has_top_level(span) {
                    Some(span.span_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(spans_marked_as_top_level, [1, 4, 5])
    }

    #[test]
    fn test_convert_trace_chunks_v04_to_v05() {
        let chunk = vec![create_test_no_alloc_span(123, 456, 789, 1, true)];

        let collection = convert_trace_chunks_v04_to_v05(&[chunk]).unwrap();

        let (dict, traces) = match collection {
            TraceChunks::V05(payload) => payload,
            _ => panic!("Unexpected type"),
        };

        assert_eq!(dict.len(), 16);

        let span = &traces[0][0];
        assert_eq!(span.service, 1);
        assert_eq!(span.name, 2);
        assert_eq!(span.resource, 3);
        assert_eq!(span.trace_id, 123);
        assert_eq!(span.span_id, 456);
        assert_eq!(span.parent_id, 789);
        assert_eq!(span.start, 1);
        assert_eq!(span.error, 0);
        assert_eq!(span.error, 0);
        assert_eq!(span.r#type, 15);
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "service").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "test-service").unwrap()
        );
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "env").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "test-env").unwrap()
        );
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "runtime-id").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "test-runtime-id-value").unwrap()
        );
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "_dd.origin").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "cloudfunction").unwrap()
        );
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "origin").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "cloudfunction").unwrap()
        );
        assert_eq!(
            *span
                .meta
                .get(&find_index_in_dict(&dict, "functionname").unwrap())
                .unwrap(),
            find_index_in_dict(&dict, "dummy_function_name").unwrap()
        );
        assert_eq!(
            *span
                .metrics
                .get(&find_index_in_dict(&dict, "_top_level").unwrap())
                .unwrap(),
            1.0
        );
    }

    #[test]
    fn test_rmp_serde_deserialize_meta_with_null_values() {
        // Create a JSON representation with null value in meta
        let span_json = json!({
            "service": "test-service",
            "name": "test_name",
            "resource": "test-resource",
            "trace_id": 1_u64,
            "span_id": 2_u64,
            "parent_id": 0_u64,
            "start": 0_i64,
            "duration": 5_i64,
            "error": 0_i32,
            "meta": {
                "service": "test-service",
                "env": "test-env",
                "runtime-id": "test-runtime-id-value",
                "problematic_key": null  // Ensure this null value does not cause an error
            },
            "metrics": {},
            "type": "",
            "meta_struct": {},
            "span_links": [],
            "span_events": []
        });

        let traces_json = vec![vec![span_json]];
        let encoded_data = rmp_serde::to_vec(&traces_json).unwrap();
        let traces: Vec<Vec<pb::Span>> = rmp_serde::from_read(&encoded_data[..])
            .expect("Failed to deserialize traces with null values in meta");

        assert_eq!(1, traces.len());
        assert_eq!(1, traces[0].len());
        let decoded_span = &traces[0][0];

        assert_eq!("test-service", decoded_span.service);
        assert_eq!("test_name", decoded_span.name);
        assert_eq!("test-resource", decoded_span.resource);
        assert_eq!("test-service", decoded_span.meta.get("service").unwrap());
        assert_eq!("test-env", decoded_span.meta.get("env").unwrap());
        assert_eq!(
            "test-runtime-id-value",
            decoded_span.meta.get("runtime-id").unwrap()
        );
        // Assert that the null value was filtered out (key not present in map)
        assert!(
            !decoded_span.meta.contains_key("problematic_key"),
            "Null value should be skipped, but key was present"
        );
    }

    #[test]
    fn test_enrich_span_with_azure_function_metadata_adds_tags_for_non_apim() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        span.name = "azure.function".to_string();

        enrich_span_with_azure_function_metadata(&mut span);

        // If AAS_METADATA_FUNCTION is available, verify aas.* tags were added
        // If not available (most test environments), this is a no-op
        // This test primarily ensures the function doesn't skip non-apim spans
        if azure_app_services::AAS_METADATA_FUNCTION.is_some() {
            assert!(span.meta.contains_key("aas.resource.id"));
            assert!(span.meta.contains_key("aas.environment.instance_id"));
            assert!(span.meta.contains_key("aas.environment.instance_name"));
            assert!(span.meta.contains_key("aas.subscription.id"));
            assert!(span.meta.contains_key("aas.environment.os"));
            assert!(span.meta.contains_key("aas.environment.runtime"));
            assert!(span.meta.contains_key("aas.environment.runtime_version"));
            assert!(span.meta.contains_key("aas.environment.function_runtime"));
            assert!(span.meta.contains_key("aas.resource.group"));
            assert!(span.meta.contains_key("aas.site.name"));
            assert!(span.meta.contains_key("aas.site.kind"));
            assert!(span.meta.contains_key("aas.site.type"));
        }
    }

    #[test]
    fn test_enrich_span_with_azure_function_metadata_skips_azure_apim() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        span.name = "azure.apim".to_string();

        enrich_span_with_azure_function_metadata(&mut span);

        // Verify no aas.* tags were added
        assert!(!span.meta.contains_key("aas.resource.id"));
        assert!(!span.meta.contains_key("aas.environment.instance_id"));
        assert!(!span.meta.contains_key("aas.environment.instance_name"));
        assert!(!span.meta.contains_key("aas.subscription.id"));
        assert!(!span.meta.contains_key("aas.environment.os"));
        assert!(!span.meta.contains_key("aas.environment.runtime"));
        assert!(!span.meta.contains_key("aas.environment.runtime_version"));
        assert!(!span.meta.contains_key("aas.environment.function_runtime"));
        assert!(!span.meta.contains_key("aas.resource.group"));
        assert!(!span.meta.contains_key("aas.site.name"));
        assert!(!span.meta.contains_key("aas.site.kind"));
        assert!(!span.meta.contains_key("aas.site.type"));
    }

    #[test]
    fn test_collect_pb_trace_chunks_searches_multiple_root_spans_for_fields() {
        // First trace root span has no fields. Second trace root span has all fields.
        // The second root span should populate all fields.
        let mut first_root_span = create_test_span(1, 1, 0, 1, true);
        first_root_span.meta.remove("env");
        first_root_span.meta.remove("runtime-id");

        let mut second_root_span = create_test_span(2, 3, 0, 1, true);
        second_root_span
            .meta
            .insert("version".to_string(), "1.2.3".to_string());
        second_root_span
            .meta
            .insert("env".to_string(), "prod".to_string());
        second_root_span
            .meta
            .insert("_dd.hostname".to_string(), "my-host".to_string());
        second_root_span
            .meta
            .insert("runtime-id".to_string(), "123".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![first_root_span], vec![second_root_span]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].app_version, "1.2.3");
        assert_eq!(payloads[0].env, "prod");
        assert_eq!(payloads[0].hostname, "my-host");
        assert_eq!(payloads[0].runtime_id, "123");
    }

    #[test]
    fn test_collect_pb_trace_chunks_searches_non_root_spans_for_fields() {
        // Root span has no fields. Child span has all fields.
        // The child span should populate all fields.
        let mut root_span = create_test_span(1, 1, 0, 1, true);
        root_span.meta.remove("env");
        root_span.meta.remove("runtime-id");
        let mut child_span = create_test_span(1, 2, 1, 1, false);
        child_span
            .meta
            .insert("version".to_string(), "1.2.3".to_string());
        child_span
            .meta
            .insert("env".to_string(), "prod".to_string());
        child_span
            .meta
            .insert("_dd.hostname".to_string(), "my-host".to_string());
        child_span
            .meta
            .insert("runtime-id".to_string(), "123".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![root_span, child_span]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].app_version, "1.2.3");
        assert_eq!(payloads[0].env, "prod");
        assert_eq!(payloads[0].hostname, "my-host");
        assert_eq!(payloads[0].runtime_id, "123");
    }

    #[test]
    fn test_collect_pb_trace_chunks_root_span_takes_priority_over_child() {
        // Root span has all fields. Child has different values for all fields.
        // The root span should populate all fields.
        let mut root_span = create_test_span(1, 1, 0, 1, true);
        root_span
            .meta
            .insert("version".to_string(), "root-version".to_string());
        root_span
            .meta
            .insert("env".to_string(), "root-env".to_string());
        root_span
            .meta
            .insert("_dd.hostname".to_string(), "root-host".to_string());
        root_span
            .meta
            .insert("runtime-id".to_string(), "root-runtime-id".to_string());

        let mut child_span = create_test_span(1, 2, 1, 1, false);
        child_span
            .meta
            .insert("version".to_string(), "child-version".to_string());
        child_span
            .meta
            .insert("env".to_string(), "child-env".to_string());
        child_span
            .meta
            .insert("_dd.hostname".to_string(), "child-host".to_string());
        child_span
            .meta
            .insert("runtime-id".to_string(), "child-runtime-id".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![root_span, child_span]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].app_version, "root-version");
        assert_eq!(payloads[0].env, "root-env");
        assert_eq!(payloads[0].hostname, "root-host");
        assert_eq!(payloads[0].runtime_id, "root-runtime-id");
    }

    #[test]
    fn test_collect_pb_trace_chunks_skips_empty_root_span_value() {
        // Root span has empty values for all fields. Child span has non-empty values.
        // The child span should populate all fields.
        let mut root_span = create_test_span(1, 1, 0, 1, true);
        root_span.meta.insert("version".to_string(), "".to_string());
        root_span.meta.insert("env".to_string(), "".to_string());
        root_span
            .meta
            .insert("_dd.hostname".to_string(), "".to_string());
        root_span
            .meta
            .insert("runtime-id".to_string(), "".to_string());

        let mut child_span = create_test_span(1, 2, 1, 1, false);
        child_span
            .meta
            .insert("version".to_string(), "1.2.3".to_string());
        child_span
            .meta
            .insert("env".to_string(), "prod".to_string());
        child_span
            .meta
            .insert("_dd.hostname".to_string(), "my-host".to_string());
        child_span
            .meta
            .insert("runtime-id".to_string(), "123".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![root_span, child_span]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].app_version, "1.2.3");
        assert_eq!(payloads[0].env, "prod");
        assert_eq!(payloads[0].hostname, "my-host");
        assert_eq!(payloads[0].runtime_id, "123");
    }

    #[test]
    fn test_collect_pb_trace_chunks_normalizes_env() {
        let mut root = create_test_span(1, 1, 0, 1, true);
        root.meta
            .insert("env".to_string(), "PRODUCTION".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![root]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].env, "production");
    }

    #[test]
    fn test_collect_pb_trace_chunks_skips_env_empty_after_normalization() {
        // First root span has an env that normalizes to empty (all invalid characters).
        // Second root span has an env should populate env fields.
        let mut first_root_span = create_test_span(1, 1, 0, 1, true);
        first_root_span
            .meta
            .insert("env".to_string(), "!!!".to_string());

        let mut second_root_span = create_test_span(2, 3, 0, 1, true);
        second_root_span
            .meta
            .insert("env".to_string(), "prod".to_string());

        let result = collect_pb_trace_chunks(
            vec![vec![first_root_span], vec![second_root_span]],
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            true,
        )
        .unwrap();

        let TracerPayloadCollection::V07(payloads) = result else {
            panic!("expected TracerPayloadCollection::V07");
        };
        assert_eq!(payloads[0].env, "prod");
    }

    #[test]
    fn test_search_trace_for_field_skips_span_with_same_id_as_root() {
        // A span with the same span_id as root is treated as the root and skipped
        // in the child span search. Only the root spans own meta is checked for it.
        let mut root = create_test_span(1, 1, 0, 1, true);
        root.meta.remove("version");

        // This span shares the same span_id as the root span, it should be skipped.
        let mut duplicate = create_test_span(1, 1, 0, 1, false);
        duplicate
            .meta
            .insert("version".to_string(), "should-not-appear".to_string());

        let trace = vec![root.clone(), duplicate];
        assert_eq!(search_trace_for_field(&root, &trace, "version"), None);
    }
}
