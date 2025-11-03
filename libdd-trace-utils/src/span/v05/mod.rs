// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::{v05::dict::SharedDict, SpanText};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

/// Structure that represent a TraceChunk Span which String fields are interned in a shared
/// dictionary. The number of elements is fixed by the spec and they all need to be serialized, in
/// case of adding more items the constant msgpack_decoder::v05::SPAN_ELEM_COUNT need to be
/// updated.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span {
    pub service: u32,
    pub name: u32,
    pub resource: u32,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: i32,
    pub meta: HashMap<u32, u32>,
    pub metrics: HashMap<u32, f64>,
    pub r#type: u32,
}

pub fn from_span<T: SpanText>(
    span: &crate::span::Span<T>,
    dict: &mut SharedDict<T>,
) -> Result<Span> {
    Ok(Span {
        service: dict.get_or_insert(&span.service)?,
        name: dict.get_or_insert(&span.name)?,
        resource: dict.get_or_insert(&span.resource)?,
        trace_id: span.trace_id,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span.meta.iter().try_fold(
            HashMap::with_capacity(span.meta.len()),
            |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
                meta.insert(dict.get_or_insert(k)?, dict.get_or_insert(v)?);
                Ok(meta)
            },
        )?,
        metrics: span.metrics.iter().try_fold(
            HashMap::with_capacity(span.metrics.len()),
            |mut metrics, (k, v)| -> anyhow::Result<HashMap<u32, f64>> {
                metrics.insert(dict.get_or_insert(k)?, *v);
                Ok(metrics)
            },
        )?,
        r#type: dict.get_or_insert(&span.r#type)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanBytes;
    use tinybytes::BytesString;

    #[test]
    fn from_span_bytes_test() {
        let span = SpanBytes {
            service: BytesString::from("service"),
            name: BytesString::from("name"),
            resource: BytesString::from("resource"),
            r#type: BytesString::from("type"),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1,
            duration: 111,
            error: 0,
            meta: HashMap::from([(
                BytesString::from("meta_field"),
                BytesString::from("meta_value"),
            )]),
            metrics: HashMap::from([(BytesString::from("metrics_field"), 1.1)]),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        };

        let mut dict = SharedDict::default();
        let v05_span = from_span(&span, &mut dict).unwrap();

        let dict = dict.dict();

        let get_index_from_str = |str: &str| -> u32 {
            dict.iter()
                .position(|s| s.as_str() == str)
                .unwrap()
                .try_into()
                .unwrap()
        };

        assert_eq!(v05_span.service, get_index_from_str("service"));
        assert_eq!(v05_span.name, get_index_from_str("name"));
        assert_eq!(v05_span.resource, get_index_from_str("resource"));
        assert_eq!(v05_span.r#type, get_index_from_str("type"));
        assert_eq!(v05_span.trace_id, 1);
        assert_eq!(v05_span.span_id, 1);
        assert_eq!(v05_span.parent_id, 0);
        assert_eq!(v05_span.start, 1);
        assert_eq!(v05_span.duration, 111);
        assert_eq!(v05_span.error, 0);
        assert_eq!(v05_span.meta.len(), 1);
        assert_eq!(v05_span.metrics.len(), 1);

        assert_eq!(
            *v05_span
                .meta
                .get(&get_index_from_str("meta_field"))
                .unwrap(),
            get_index_from_str("meta_value")
        );
        assert_eq!(
            *v05_span
                .metrics
                .get(&get_index_from_str("metrics_field"))
                .unwrap(),
            1.1
        );
    }
}
