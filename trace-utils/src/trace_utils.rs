// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use crate::send_data::send_data_result::SendDataResult;
pub use crate::send_data::SendData;
use crate::span::v05;
use crate::span::v05::dict::SharedDict;
pub use crate::tracer_header_tags::TracerHeaderTags;
use crate::tracer_payload;
use crate::tracer_payload::{TraceCollection, TracerPayloadCollection};
use anyhow::anyhow;
use bytes::buf::Reader;
use datadog_trace_normalization::normalizer;
use datadog_trace_protobuf::pb;
use ddcommon::{azure_app_services, hyper_migration};
use http_body_util::BodyExt;
use hyper::body::Buf;
use log::error;
use rmp::decode::read_array_len;
use rmpv::decode::read_value;
use rmpv::{Integer, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::env;

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
const MEASURED_KEY: &str = "_dd.measured";
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";

const MAX_PAYLOAD_SIZE: usize = 50 * 1024 * 1024;
const MAX_STRING_DICT_SIZE: u32 = 25_000_000;
const SPAN_ELEMENT_COUNT: usize = 12;

/// First value of returned tuple is the payload size
pub async fn get_traces_from_request_body(
    body: hyper_migration::Body,
) -> anyhow::Result<(usize, Vec<Vec<pb::Span>>)> {
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
                let parsed_string = s.into_str().ok_or_else(|| anyhow!("Error reading string dict"))?;
                dict.push(parsed_string);
            }
            val => anyhow::bail!("Error deserializing strings dictionary. Value in string dict is not a string: {val}")
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
    //0 - service
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
    //6 - start
    match read_value(reader)? {
        Value::Integer(i) => {
            span.start = i
                .as_i64()
                .ok_or_else(|| anyhow!("Error reading span start, value is not an integer: {i}"))?;
        }
        val => anyhow::bail!("Error reading span start, value is not an integer: {val}"),
    };
    //7 - duration
    match read_value(reader)? {
        Value::Integer(i) => {
            span.duration = i.as_i64().ok_or_else(|| {
                anyhow!("Error reading span duration, value is not an integer: {i}")
            })?;
        }
        val => anyhow::bail!("Error reading span duration, value is not an integer: {val}"),
    };
    //8 - error
    match read_value(reader)? {
        Value::Integer(i) => {
            span.error = i
                .as_i64()
                .ok_or_else(|| anyhow!("Error reading span error, value is not an integer: {i}"))?
                as i32;
        }
        val => anyhow::bail!("Error reading span error, value is not an integer: {val}"),
    }
    //9 - meta
    match read_value(reader)? {
        Value::Map(meta) => {
            for (k, v) in meta.iter() {
                match k {
                    Value::Integer(k) => {
                        match v {
                            Value::Integer(v) => {
                                let key = str_from_dict(dict, *k)?;
                                let val = str_from_dict(dict, *v)?;
                                span.meta.insert(key, val);
                            }
                            _ => anyhow::bail!("Error reading span meta, value is not an integer and can't be looked up in dict: {v}")
                        }
                    }
                    _ => anyhow::bail!("Error reading span meta, key is not an integer and can't be looked up in dict: {k}")
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
        Value::Integer(s) => {
            str_from_dict(dict, s)
        },
        val => anyhow::bail!("Error reading {field_name}, value is not an integer and can't be looked up in dict: {val}")
    }
}

pub async fn get_v05_traces_from_request_body(
    body: hyper_migration::Body,
) -> anyhow::Result<(usize, Vec<Vec<pb::Span>>)> {
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

/// Tags gathered from a trace's root span
#[derive(Default)]
pub struct RootSpanTags<'a> {
    pub env: &'a str,
    pub app_version: &'a str,
    pub hostname: &'a str,
    pub runtime_id: &'a str,
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
}

pub fn coalesce_send_data(mut data: Vec<SendData>) -> Vec<SendData> {
    // TODO trace payloads with identical data except for chunk could be merged?

    data.sort_unstable_by(|a, b| {
        a.get_target()
            .url
            .to_string()
            .cmp(&b.get_target().url.to_string())
            .then(a.get_target().test_token.cmp(&b.get_target().test_token))
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
                // Note: dedup_by drops a, and retains b.
                b.tracer_payloads.append(&mut a.tracer_payloads);
                b.size += a.size;
                return true;
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

pub fn get_root_span_index(trace: &[pb::Span]) -> anyhow::Result<usize> {
    if trace.is_empty() {
        anyhow::bail!("Cannot find root span index in an empty trace.");
    }

    // Do a first pass to find if we have an obvious root span (starting from the end) since some
    // clients put the root span last.
    for (i, span) in trace.iter().enumerate().rev() {
        if span.parent_id == 0 {
            return Ok(i);
        }
    }

    let mut span_ids: HashSet<u64> = HashSet::with_capacity(trace.len());
    for span in trace.iter() {
        span_ids.insert(span.span_id);
    }

    let mut root_span_id = None;
    for (i, span) in trace.iter().enumerate() {
        // If a span's parent is not in the trace, it is a root
        if !span_ids.contains(&span.parent_id) {
            if root_span_id.is_some() {
                error!(
                    "trace has multiple root spans trace_id: {}",
                    &trace[0].trace_id
                );
            }
            root_span_id = Some(i);
        }
    }
    Ok(match root_span_id {
        Some(i) => i,
        None => {
            error!(
                "Could not find the root span for trace with trace_id: {}",
                &trace[0].trace_id,
            );
            trace.len() - 1
        }
    })
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

/// Return true if the span has a top level key set
pub fn has_top_level(span: &pb::Span) -> bool {
    span.metrics
        .get(TRACER_TOP_LEVEL_KEY)
        .is_some_and(|v| *v == 1.0)
        || span.metrics.get(TOP_LEVEL_KEY).is_some_and(|v| *v == 1.0)
}

fn set_top_level_span(span: &mut pb::Span, is_top_level: bool) {
    if is_top_level {
        span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
    } else {
        span.metrics.remove(TOP_LEVEL_KEY);
    }
}

pub fn set_serverless_root_span_tags(
    span: &mut pb::Span,
    app_name: Option<String>,
    env_type: &EnvironmentType,
) {
    span.r#type = "serverless".to_string();
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
    let Some(region) = &mini_agent_metadata.gcp_region
    else {
        todo!()
    };
    #[allow(clippy::todo)]
    let Some(project) = &mini_agent_metadata.gcp_project_id
    else {
        todo!()
    };

    #[allow(clippy::unwrap_used)]
    if function.is_some() && !region.is_empty() && !project.is_empty() {
        let resource_name = format!(
            "projects/{}/locations/{}/functions/{}",
            project,
            region,
            function.unwrap()
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
    if let Some(aas_metadata) = azure_app_services::get_function_metadata() {
        let aas_tags = [
            ("aas.resource.id", aas_metadata.get_resource_id()),
            (
                "aas.environment.instance_id",
                aas_metadata.get_instance_id(),
            ),
            (
                "aas.environment.instance_name",
                aas_metadata.get_instance_name(),
            ),
            ("aas.subscription.id", aas_metadata.get_subscription_id()),
            ("aas.environment.os", aas_metadata.get_operating_system()),
            ("aas.environment.runtime", aas_metadata.get_runtime()),
            (
                "aas.environment.runtime_version",
                aas_metadata.get_runtime_version(),
            ),
            (
                "aas.environment.function_runtime",
                aas_metadata.get_function_runtime_version(),
            ),
            ("aas.resource.group", aas_metadata.get_resource_group()),
            ("aas.site.name", aas_metadata.get_site_name()),
            ("aas.site.kind", aas_metadata.get_site_kind()),
            ("aas.site.type", aas_metadata.get_site_type()),
        ];
        aas_tags.into_iter().for_each(|(name, value)| {
            span.meta.insert(name.to_string(), value.to_string());
        });
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

pub fn collect_trace_chunks<T: tracer_payload::TraceChunkProcessor>(
    traces: TraceCollection,
    tracer_header_tags: &TracerHeaderTags,
    process_chunk: &mut T,
    is_agentless: bool,
    use_v05_format: bool,
) -> anyhow::Result<TracerPayloadCollection> {
    match traces {
        TraceCollection::TraceChunk(traces) => {
            if use_v05_format {
                let mut shared_dict = SharedDict::default();
                let mut v05_traces: Vec<Vec<v05::Span>> = Vec::with_capacity(traces.len());
                for trace in traces {
                    let v05_trace = trace.iter().try_fold(
                        Vec::with_capacity(trace.len()),
                        |mut acc, span| -> anyhow::Result<Vec<v05::Span>> {
                            acc.push(v05::from_span_bytes(span, &mut shared_dict)?);
                            Ok(acc)
                        },
                    )?;

                    v05_traces.push(v05_trace);
                }
                Ok(TracerPayloadCollection::V05((
                    shared_dict.dict(),
                    v05_traces,
                )))
            } else {
                Ok(TracerPayloadCollection::V04(traces))
            }
        }
        TraceCollection::V07(mut traces) => {
            let mut trace_chunks: Vec<pb::TraceChunk> = Vec::new();

            // We'll skip setting the global metadata and rely on the agent to unpack these
            let mut gathered_root_span_tags = !is_agentless;
            let mut root_span_tags = RootSpanTags::default();

            for trace in traces.iter_mut() {
                if is_agentless {
                    if let Err(e) = normalizer::normalize_trace(trace) {
                        error!("Error normalizing trace: {e}");
                    }
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

                process_chunk.process(&mut chunk, root_span_index);

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

            Ok(TracerPayloadCollection::V07(vec![
                construct_tracer_payload(trace_chunks, tracer_header_tags, root_span_tags),
            ]))
        }
    }
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
    use crate::test_utils::{create_test_no_alloc_span, create_test_span};
    use ddcommon::Endpoint;
    use hyper::Request;
    use serde_json::json;
    use tinybytes::BytesString;

    fn find_index_in_dict(dict: &[BytesString], value: &str) -> Option<u32> {
        let idx = dict.iter().position(|e| e.as_str() == value);
        idx.map(|idx| idx.try_into().unwrap())
    }

    #[test]
    fn test_coalescing_does_not_exceed_max_size() {
        let dummy = SendData::new(
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
            }]),
            TracerHeaderTags::default(),
            &Endpoint::default(),
        );
        let coalesced = coalesce_send_data(vec![
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
        ]);
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
        let res = get_v05_traces_from_request_body(hyper_migration::Body::from(bytes)).await;
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
                }]],
            ),
        ];

        for (trace_input, output) in pairs {
            let bytes = rmp_serde::to_vec(&vec![&trace_input]).unwrap();
            let request = Request::builder()
                .body(hyper_migration::Body::from(bytes))
                .unwrap();
            let res = get_traces_from_request_body(request.into_body()).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap().1, output);
        }
    }

    #[tokio::test]
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
        }]];

        let bytes = rmp_serde::to_vec(&trace_input).unwrap();
        let request = Request::builder()
            .body(hyper_migration::Body::from(bytes))
            .unwrap();

        let res = get_traces_from_request_body(request.into_body()).await;
        assert!(res.is_ok(), "Failed to deserialize: {:?}", res);
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
        assert_eq!(span.r#type, "serverless".to_string())
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
        assert_eq!(span.r#type, "serverless".to_string())
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
    fn test_collect_trace_chunks_v05() {
        let chunk = vec![create_test_no_alloc_span(123, 456, 789, 1, true)];

        let collection = collect_trace_chunks(
            TraceCollection::TraceChunk(vec![chunk]),
            &TracerHeaderTags::default(),
            &mut tracer_payload::DefaultTraceChunkProcessor,
            false,
            true,
        )
        .unwrap();

        let (dict, traces) = match collection {
            TracerPayloadCollection::V05(payload) => payload,
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
}
