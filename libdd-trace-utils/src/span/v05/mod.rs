// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::{v05::dict::SharedDict, TraceData, TraceProjector, TraceValueType, TraceValue, TraceValueOp, Traces, SpanValue, SpanValueType, TraceAttributes, TraceAttributesOp, AttributeAnyValue, AttributeAnyValueType, AttributeInnerValue, MUT, TraceValueMutOp};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::ops::Deref;
use std::slice::Iter;
use crate::span::table::{StaticDataVec, TraceDataText, TraceStringRef};

/// Structure that represent a TraceChunk Span which String fields are interned in a shared
/// dictionary. The number of elements is fixed by the spec and they all need to be serialized, in
/// case of adding more items the constant msgpack_decoder::v05::SPAN_ELEM_COUNT need to be
/// updated.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Span {
    pub service: TraceStringRef,
    pub name: TraceStringRef,
    pub resource: TraceStringRef,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: i32,
    pub meta: HashMap<TraceStringRef, TraceStringRef>,
    pub metrics: HashMap<TraceStringRef, f64>,
    pub r#type: TraceStringRef,
}

pub struct ChunkCollection<T: TraceData> {
    pub dict: StaticDataVec<T, TraceDataText>,
    pub chunks: Vec<Vec<Span>>,
}

impl<'a, D: TraceData> TraceProjector<D> for &'a ChunkCollection<D> {
    type Storage = StaticDataVec<D, TraceDataText>;
    type TraceRef = Vec<Vec<Span>>;
    type ChunkRef = Vec<Span>;
    type SpanRef = Span;
    type SpanLinkRef = ();
    type SpanEventRef = ();
    type AttributeRef = Span;

    fn project(self) -> Traces<Self::Storage, D> {
        Traces {
            storage: self,
        }
    }

    fn chunk_iterator(trace: Self::TraceRef) -> Iter<Vec<Span>> {
        trace.chunks.iter()
    }

    fn span_iterator(chunk: Self::ChunkRef) -> Iter<Span> {
        chunk.iter()
    }

    fn span_link_iterator(span: Self::SpanRef) -> Iter<<Self::SpanLinkRef as Deref>::Target> {
        [].iter()
    }

    fn span_events_iterator(span: Self::SpanRef) -> Iter<<Self::SpanEventRef as Deref>::Target> {
        [].iter()
    }
}

impl<'a, D: TraceData> TraceValueOp<D> for TraceValue<&'a mut ChunkCollection<D>, D, { TraceValueType::ContainerId as u8 }> {
    fn set<I: Into<Self::Value>>(&self, value: I) {
        todo!()
    }

    fn get(&self) -> &str {
        todo!()
    }
}

impl<'a, D: TraceData> TraceValueMutOp<D> for SpanValue<&'a ChunkCollection<D>, D, { SpanValueType::Service as u8 }, MUT> {
    fn set(storage: &mut &'a mut StaticDataVec<D, TraceDataText>, span: &'a mut Span, value: D::Text) {
        storage.update(&mut span.service, value)
    }
}

impl<'a, D: TraceData> TraceValueOp<D> for SpanValue<&'a ChunkCollection<D>, D, { SpanValueType::Service as u8 }> {
    fn get(&self) -> D::Text {
        self.storage.get(self.span.service)
    }
}

fn inner_value<'a, D: TraceData, const Type: u8>(span: &'a mut Span, dict: &'a mut StaticDataVec<D, TraceDataText>) -> AttributeInnerValue<&'a ChunkCollection<D>, D, Type> {
    AttributeInnerValue {
        storage: dict,
        container: span,
    }
}

impl<'a, D: TraceData> TraceAttributesOp<&'a ChunkCollection<D>, D> for TraceAttributes<&'a ChunkCollection<D>, D, &'a ChunkCollection<D>> {
    fn set(&self, key: &str, value: AttributeAnyValueType) -> AttributeAnyValue<&'a ChunkCollection<D>, D> {
        let span = &mut self.container.chunks[0][0];
        match value {
            AttributeAnyValueType::String => AttributeAnyValue::String(inner_value(span, self.storage)),
            AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(inner_value(span, self.storage)),
            AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(inner_value(span, self.storage)),
            AttributeAnyValueType::Integer => AttributeAnyValue::Integer(inner_value(span, self.storage)),
            AttributeAnyValueType::Double => AttributeAnyValue::Double(inner_value(span, self.storage)),
            AttributeAnyValueType::Array => {}
            AttributeAnyValueType::Map => {}
        }
    }

    fn get(&self, key: &str) -> Option<AttributeAnyValue<&'a ChunkCollection<D>, D>> {
        todo!()
    }

    fn remove(&self, key: &str) {
        todo!()
    }
}

impl<T: TraceProjector<D>, D: TraceData> TraceAttributes<T, D> {
    pub fn set(&self, key: &str, value: AttributeAnyValue) {
        self.container.set_attribute(self, key, value);
    }
}


pub fn from_v04_span<T: TraceData>(
    span: crate::span::v04::Span<T>,
    dict: &mut SharedDict<T::Text>,
) -> Result<Span> {
    /*
    let meta_len = span.meta.len();
    let metrics_len = span.metrics.len();
    Ok(Span {
        service: dict.get_or_insert(span.service)?,
        name: dict.get_or_insert(span.name)?,
        resource: dict.get_or_insert(span.resource)?,
        trace_id: span.trace_id as u64,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span.meta.into_iter().try_fold(
            HashMap::with_capacity(meta_len),
            |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
                meta.insert(dict.get_or_insert(k)?, dict.get_or_insert(v)?);
                Ok(meta)
            },
        )?,
        metrics: span.metrics.into_iter().try_fold(
            HashMap::with_capacity(metrics_len),
            |mut metrics, (k, v)| -> anyhow::Result<HashMap<u32, f64>> {
                metrics.insert(dict.get_or_insert(k)?, v);
                Ok(metrics)
            },
        )?,
        r#type: dict.get_or_insert(span.r#type)?,
    })

     */
    Ok(Span::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::SpanBytes;
    use libdd_tinybytes::BytesString;

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
        let v05_span = from_v04_span(span, &mut dict).unwrap();

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
