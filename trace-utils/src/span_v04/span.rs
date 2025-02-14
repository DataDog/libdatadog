// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use tinybytes::{Bytes, BytesString};

#[derive(Debug, PartialEq)]
pub enum SpanKey {
    Service,
    Name,
    Resource,
    TraceId,
    SpanId,
    ParentId,
    Start,
    Duration,
    Error,
    Meta,
    Metrics,
    Type,
    MetaStruct,
    SpanLinks,
}

impl FromStr for SpanKey {
    type Err = SpanKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "service" => Ok(SpanKey::Service),
            "name" => Ok(SpanKey::Name),
            "resource" => Ok(SpanKey::Resource),
            "trace_id" => Ok(SpanKey::TraceId),
            "span_id" => Ok(SpanKey::SpanId),
            "parent_id" => Ok(SpanKey::ParentId),
            "start" => Ok(SpanKey::Start),
            "duration" => Ok(SpanKey::Duration),
            "error" => Ok(SpanKey::Error),
            "meta" => Ok(SpanKey::Meta),
            "metrics" => Ok(SpanKey::Metrics),
            "type" => Ok(SpanKey::Type),
            "meta_struct" => Ok(SpanKey::MetaStruct),
            "span_links" => Ok(SpanKey::SpanLinks),
            _ => Err(SpanKeyParseError::new(format!("Invalid span key: {}", s))),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span {
    pub service: BytesString,
    pub name: BytesString,
    pub resource: BytesString,
    pub r#type: BytesString,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    #[serde(skip_serializing_if = "is_default")]
    pub error: i32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<BytesString, BytesString>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub metrics: HashMap<BytesString, f64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta_struct: HashMap<BytesString, Vec<u8>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLink>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SpanLink {
    pub trace_id: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    pub attributes: HashMap<BytesString, BytesString>,
    pub tracestate: BytesString,
    pub flags: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpanSlice<'a> {
    pub service: &'a str,
    pub name: &'a str,
    pub resource: &'a str,
    pub r#type: &'a str,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    #[serde(skip_serializing_if = "is_default")]
    pub error: i32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<&'a str, &'a str>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub metrics: HashMap<&'a str, f64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta_struct: HashMap<&'a str, Vec<u8>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLinkSlice<'a>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SpanLinkSlice<'a> {
    pub trace_id: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    pub attributes: HashMap<&'a str, &'a str>,
    pub tracestate: &'a str,
    pub flags: u64,
}

impl SpanSlice<'_> {
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<Span> {
        Some(Span {
            service: BytesString::try_from_bytes_slice(bytes, self.service)?,
            name: BytesString::try_from_bytes_slice(bytes, self.name)?,
            resource: BytesString::try_from_bytes_slice(bytes, self.resource)?,
            r#type: BytesString::try_from_bytes_slice(bytes, self.r#type)?,
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_id: self.parent_id,
            start: self.start,
            duration: self.duration,
            error: self.error,
            meta: self
                .meta
                .iter()
                .map(|(k, v)| {
                    Some((
                        BytesString::try_from_bytes_slice(bytes, k)?,
                        BytesString::try_from_bytes_slice(bytes, v)?,
                    ))
                })
                .collect::<Option<HashMap<BytesString, BytesString>>>()?,
            metrics: self
                .metrics
                .iter()
                .map(|(k, v)| Some((BytesString::try_from_bytes_slice(bytes, k)?, *v)))
                .collect::<Option<HashMap<BytesString, f64>>>()?,
            meta_struct: self
                .meta_struct
                .iter()
                .map(|(k, v)| Some((BytesString::try_from_bytes_slice(bytes, k)?, v.clone())))
                .collect::<Option<HashMap<BytesString, Vec<u8>>>>()?,
            span_links: self
                .span_links
                .iter()
                .map(|link| link.try_to_bytes(bytes))
                .collect::<Option<Vec<SpanLink>>>()?,
        })
    }
}

impl SpanLinkSlice<'_> {
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<SpanLink> {
        Some(SpanLink {
            trace_id: self.trace_id,
            trace_id_high: self.trace_id_high,
            span_id: self.span_id,
            attributes: self
                .attributes
                .iter()
                .map(|(k, v)| {
                    Some((
                        BytesString::try_from_bytes_slice(bytes, k)?,
                        BytesString::try_from_bytes_slice(bytes, v)?,
                    ))
                })
                .collect::<Option<HashMap<BytesString, BytesString>>>()?,
            tracestate: BytesString::try_from_bytes_slice(bytes, self.tracestate)?,
            flags: self.flags,
        })
    }
}

#[derive(Debug)]
pub struct SpanKeyParseError {
    pub message: String,
}

impl SpanKeyParseError {
    pub fn new(message: impl Into<String>) -> Self {
        SpanKeyParseError {
            message: message.into(),
        }
    }
}
impl fmt::Display for SpanKeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpanKeyParseError: {}", self.message)
    }
}
impl std::error::Error for SpanKeyParseError {}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}
