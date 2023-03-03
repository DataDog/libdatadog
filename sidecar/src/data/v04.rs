use std::{
    borrow::{Borrow, Cow},
    collections::HashMap,
};

use datadog_trace_protobuf::pb;
use hyper::HeaderMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Span<'a> {
    #[serde(borrow)]
    service: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    resource: Cow<'a, str>,
    trace_id: u64,
    span_id: u64,
    parent_id: Option<u64>,
    start: i64,
    duration: i64,
    error: i32,
    #[serde(borrow)]
    meta: HashMap<&'a str, &'a str>,
    #[serde(borrow)]
    metrics: HashMap<&'a str, f64>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Trace<'a> {
    #[serde(borrow)]
    spans: Vec<Span<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Payload<'a> {
    #[serde(borrow)]
    pub traces: Vec<Trace<'a>>,
}

impl<'a> Payload<'a> {
    pub fn find_top_meta(&self, key: &str) -> Option<Cow<'a, str>> {
        self.traces
            .iter()
            .flat_map(|s| s.spans.first()) // only look at first span
            .flat_map(|s| s.meta.get(key))
            .next()
            .map(|s| Cow::Borrowed(*s))
    }
}

#[derive(Clone, Debug)]
pub struct AssemblerBuilder {
    hostname: String,
    env: String,
    tags: HashMap<String, String>,
}

impl Default for AssemblerBuilder {
    fn default() -> Self {
        Self { hostname: "".into(), env: "".into(), tags: Default::default() } //TODO: read tags from env 
    }
}


impl AssemblerBuilder {
    fn val_from_header<'a>(headers: &'a HeaderMap, key: &str) -> Option<Cow<'a, str>> {
        headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .map(|s| Cow::Borrowed(s))
    }

    pub fn with_headers<'a>(&'a self, headers: &'a HeaderMap) -> TracerPayloadAssembler<'a> {
        TracerPayloadAssembler {
            container_id: Self::val_from_header(headers, "datadog-container-id"),
            language_name: Self::val_from_header(headers, "datadog-meta-lang"),
            language_version: Self::val_from_header(headers, "datadog-meta-lang-version"),
            tracer_version: Self::val_from_header(headers, "datadog-meta-tracer-version"),
            env: Some(self.env.as_str().into()),
            hostname: self.hostname.as_str().into(),
            tags: &self.tags,
        }
    }
}

pub struct TracerPayloadAssembler<'a> {
    container_id: Option<Cow<'a, str>>,
    language_name: Option<Cow<'a, str>>,
    language_version: Option<Cow<'a, str>>,
    tracer_version: Option<Cow<'a, str>>,
    env: Option<Cow<'a, str>>,
    hostname: &'a str,
    tags: &'a HashMap<String, String>,
}

impl<'a> From<Span<'a>> for pb::Span {
    fn from(src: Span<'a>) -> Self {
        pb::Span {
            service: src.service.map(Cow::into_owned).unwrap_or_default(),
            name: src.name.to_string(),
            resource: src.resource.to_string(),
            trace_id: src.trace_id,
            span_id: src.span_id,
            parent_id: src.parent_id.unwrap_or(0),
            start: src.start,
            duration: src.duration,
            error: src.error,
            meta: src
                .meta
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            metrics: src
                .metrics
                .iter()
                .map(|(key, value)| (key.to_string(), *value))
                .collect(),
            r#type: "custom".into(),            // TODO: ?
            meta_struct: Default::default(), //TODO: ?
        }
    }
}

impl<'a> TracerPayloadAssembler<'a> {
    fn allemble_chunk<'b>(&self, src: Trace<'b>) -> pb::TraceChunk {
        pb::TraceChunk {
            priority: 1,       // TODO: ?
            origin: "sidecar-origin".into(), //TODO: ?
            spans: src.spans.into_iter().map(Into::into).collect(),
            tags: Default::default(),
            dropped_trace: false, //TODO: ?
        }
    }

    pub fn assemble_payload<'b>(&self, src: Payload<'b>) -> pb::TracerPayload {
        let rt_id = src.find_top_meta("runtime-id");
        let env = src.find_top_meta("env"); //TODO override from settings
        let version = src.find_top_meta("version");

        pb::TracerPayload {
            container_id: self
                .container_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            language_name: self
                .language_name
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            language_version: self
                .language_version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            tracer_version: self
                .tracer_version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            runtime_id: rt_id.as_ref().map(ToString::to_string).unwrap_or_default(),
            chunks: src
                .traces
                .into_iter()
                .map(|t| (&self).allemble_chunk(t))
                .collect(),
            tags: self.tags.clone(),
            env: env.as_ref().map(ToString::to_string).unwrap_or_default(),
            hostname: self.hostname.to_owned(),
            app_version: version.as_ref().map(ToString::to_string).unwrap_or_default(),
        }
    }
}

impl<'a> From<&'a pb::Span> for Span<'a> {
    fn from(value: &'a pb::Span) -> Self {
        let pb::Span {
            service,
            name,
            resource,
            trace_id,
            span_id,
            parent_id,
            start,
            duration,
            error,
            meta,
            metrics,
            ..
        } = value;

        Self {
            service: Some(service.into()),
            name: name.into(),
            resource: resource.into(),
            trace_id: *trace_id,
            span_id: *span_id,
            parent_id: Some(*parent_id),
            start: *start,
            duration: *duration,
            error: *error,
            meta: meta.iter().map(|(k,v)| (k.as_str(), v.as_str())).collect(),
            metrics: metrics.iter().map(|(k,v)| (k.as_str(), *v)).collect(),
        }
    }
}


impl<'a> From<&'a pb::TraceChunk> for Trace<'a> {
    fn from(value: &'a pb::TraceChunk) -> Self {
        Self {
            spans: value.spans.iter().map(Into::into).collect(),
        }
    }
}

impl<'a> From<&'a pb::TracerPayload> for Payload<'a> {
    fn from(value: &'a pb::TracerPayload) -> Self {
        Self {
            traces: value.chunks.iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::HashMap};

    use crate::data::v04::{Payload, Span, Trace};

    #[test]
    fn test_borrow_when_deserializing() {
        let data_orig = Payload {
            traces: vec![Trace {
                spans: vec![Span {
                    service: Some("service".into()),
                    name: "name".into(),
                    resource: "resource".into(),
                    trace_id: 1,
                    span_id: 2,
                    parent_id: None,
                    start: 4,
                    duration: 5,
                    error: 1,
                    meta: HashMap::from([("key", "value")]),
                    metrics: HashMap::from([("metric", 0.1)]),
                }],
            }],
        };

        let buf = rmp_serde::to_vec(&data_orig).expect("serialize");
        let data_new: Payload = rmp_serde::from_slice(&buf).expect("deserialize");

        // Validate data in deserialized payload, is borrowed from buffer
        // where possible to avoid unnecessary allocations when processing incoming data
        //
        // note, serde borrow deserialization has some edgecases with nested types
        // best to check if things are actually borrowed here.
        let span = &data_new.traces[0].spans[0];

        // Option<Cow> is not borrowed by default
        // TODO: use https://docs.rs/serde_with/latest/serde_with/struct.BorrowCow.html or remove Cow
        assert!(matches!(span.service, Some(Cow::Owned(_))));

        assert!(buf.as_ptr_range().contains(&span.name.as_ptr()));
        assert!(matches!(span.name, Cow::Borrowed(_)));
        assert!(matches!(span.resource, Cow::Borrowed(_)));

        for (k, v) in &span.meta {
            assert!(buf.as_ptr_range().contains(&k.as_ptr()));
            assert!(buf.as_ptr_range().contains(&v.as_ptr()));
        }
    }
}
