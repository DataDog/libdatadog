// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use crate::span::{v05::dict::SharedDict, TraceData, TraceProjector, Traces, SpanValue, TraceAttributes, TraceAttributesOp, AttributeAnyValue, TracesMut, parse_span_kind, span_kind_to_str, AttributeAnyContainer, TraceAttributesMutOp, TraceAttributesMut, AttributeAnyValueType, TraceAttributesString, TraceAttributesBytes, AttributeAnySetterContainer, AttributeAnyGetterContainer, TraceAttributesBoolean, TraceAttributesInteger, TraceAttributesDouble, SpanBytes, SpanText, AttrOwned, AttrRef};
use anyhow::Result;
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::slice::Iter;
use datadog_trace_protobuf::pb::idx::SpanKind;
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

impl Span {
    fn set_trace_id<D: TraceData>(&mut self, trace_id: u128, storage: &mut Storage<D>) {
        self.trace_id = trace_id as u64;
        if trace_id >> 64 > 0 {
            self.set_meta("_dd.p.tid", storage, format!("{:016x}", (trace_id >> 64) as u64));
        } else {
            self.remove_meta("_dd.p.tid", storage);
        }
    }

    fn get_meta<'a, D: TraceData, K: Into<D::Text>>(&self, key: K, storage: &'a Storage<D>) -> Option<&'a D::Text> {
        storage.find(key.into()).and_then(|r| self.meta.get(&r)).map(|r| r.get(storage))
    }

    fn set_meta<D: TraceData, K: Into<D::Text>, V: Into<D::Text>>(&mut self, key: K, storage: &mut Storage<D>, value: V) -> &mut TraceStringRef {
        let r = storage.add(key.into());
        let value = storage.add(value.into());
        match self.meta.entry(r) {
            Entry::Occupied(mut e) => {
                storage.decref(r);
                storage.decref(e.insert(value));
                e.into_mut()
            }
            Entry::Vacant(e) => e.insert(value),
        }
    }

    fn remove_meta<D: TraceData, K: Into<D::Text>>(&mut self, key: K, storage: &mut Storage<D>) {
        if let Some(r) = storage.find(key.into()) {
            if let Some(removed) = self.meta.remove(&r) {
                storage.decref(r);
                storage.decref(removed);
            }
        }
    }

    fn get_metric<D: TraceData, K: Into<D::Text>>(&self, key: K, storage: &Storage<D>) -> Option<f64> {
        storage.find(key).and_then(|r| self.metrics.get(&r).map(|v| *v))
    }

    fn set_metric<D: TraceData, K: Into<D::Text>>(&mut self, key: K, storage: &mut Storage<D>, value: f64) -> &mut f64 {
        let r = storage.add(key);
        match self.metrics.entry(r) {
            Entry::Occupied(mut e) => {
                storage.decref(r);
                e.insert(value);
                e.into_mut()
            }
            Entry::Vacant(e) => e.insert(value),
        }
    }

    fn remove_metric<D: TraceData, K: Into<D::Text>>(&mut self, key: K, storage: &mut Storage<D>) {
        if let Some(r) = storage.find(key) {
            if self.meta.remove(&r).is_some() {
                storage.decref(r);
            }
        }
    }
}

type Storage<D> = StaticDataVec<D, TraceDataText>;

pub struct ChunkCollection<D: TraceData> {
    pub dict: Storage<D>,
    pub chunks: Vec<Vec<Span>>,
    // TODO: collect header data here
}

fn free_span_data<D: TraceData>(span: &mut Span, storage: &mut Storage<D>) {
    span.service.reset(storage);
    span.name.reset(storage);
    span.resource.reset(storage);
    span.r#type.reset(storage);
    for (mut key, mut value) in std::mem::take(&mut span.meta).into_iter() {
        key.reset(storage);
        value.reset(storage);
    }
    for (mut key, _) in std::mem::take(&mut span.meta).into_iter() {
        key.reset(storage)
    }
}

fn free_chunk_data<D: TraceData>(chunk: &mut Vec<Span>, storage: &mut Storage<D>) {
    for mut span in std::mem::take(chunk).into_iter() {
        free_span_data(&mut span, storage);
    }
}

fn find_chunk_root_span() {
    // TODO: we should probably cache this?!

}

impl<D: TraceData> TraceProjector<D> for ChunkCollection<D> {
    type Storage = StaticDataVec<D, TraceDataText>;
    type Trace = Vec<Vec<Span>>;
    type Chunk = Vec<Span>;
    type Span = Span;
    type SpanLink = [(); 0];
    type SpanEvent = [(); 0];

    fn project(&self) -> Traces<Self, D> {
        Traces::new(&self.chunks, &self.dict)
    }

    fn project_mut(&mut self) -> TracesMut<Self, D> {
        Traces::new_mut(&mut self.chunks, &mut self.dict)
    }

    fn add_chunk<'a>(trace: &'a mut Self::Trace, _storage: &mut Self::Storage) -> &'a mut Self::Chunk {
        trace.push(Vec::new());
        unsafe { trace.last_mut().unwrap_unchecked() }
    }

    fn chunk_iterator(trace: &Self::Trace) -> Iter<Vec<Span>> {
        trace.iter()
    }

    fn retain_chunks(trace: &mut Self::Trace, storage: &mut Self::Storage, mut predicate: impl FnMut(&mut Self::Chunk, &mut Self::Storage) -> bool) {
        trace.retain_mut(|chunk| {
            if predicate(chunk, storage) {
                true
            } else {
                free_chunk_data(chunk, storage);
                false
            }
        })
    }

    fn add_span<'a>(chunk: &'a mut Self::Chunk, storage: &mut Self::Storage) -> &'a mut Self::Span {
        // TODO: well, we can optimize that and directly copy instead of always re-computing the trace_id
        let trace_id = Self::get_chunk_trace_id(chunk, storage);
        chunk.push(Span::default());
        let span = unsafe { chunk.last_mut().unwrap_unchecked() };
        span.set_trace_id(trace_id, storage);
        span
    }

    fn span_iterator(chunk: &Self::Chunk) -> Iter<Span> {
        chunk.iter()
    }

    fn retain_spans(chunk: &mut Self::Chunk, storage: &mut Self::Storage, mut predicate: impl FnMut(&mut Self::Span, &mut Self::Storage) -> bool) {
        chunk.retain_mut(|span| {
            if predicate(span, storage) {
                true
            } else {
                free_span_data(span, storage);
                false
            }
        })
    }

    fn add_span_link<'a>(_span: &'a mut Self::Span, _storage: &mut Self::Storage) -> &'a mut Self::SpanLink {
        &mut []
    }

    fn span_link_iterator(_span: &Self::Span) -> Iter<Self::SpanLink> {
        [].iter()
    }

    fn retain_span_links(_trace: &mut Self::Span, _storage: &mut Self::Storage, _predicate: impl FnMut(&mut Self::SpanLink, &mut Self::Storage) -> bool) {
    }

    fn add_span_event<'a>(_span: &mut Self::Span, _storage: &mut Self::Storage) -> &'a mut Self::SpanEvent {
        &mut []
    }

    fn span_event_iterator(_span: &Self::Span) -> Iter<Self::SpanEvent> {
        [].iter()
    }

    fn retain_span_events(_trace: &mut Self::Span, _storage: &mut Self::Storage, _predicate: impl FnMut(&mut Self::SpanEvent, &mut Self::Storage) -> bool) {
    }

    fn get_trace_container_id<'a>(_trace: &'a Self::Trace, _storage: &'a Self::Storage) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_language_name<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_language_version<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_tracer_version<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_runtime_id<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_env<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_hostname<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_trace_app_version<'a>(trace: &'a Self::Trace, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn set_trace_container_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_language_name(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_language_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_tracer_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_runtime_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_env(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_hostname(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_trace_app_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn get_chunk_priority(chunk: &Self::Chunk, storage: &Self::Storage) -> i32 {
        todo!()
    }

    fn get_chunk_origin<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> &'a D::Text {
        todo!()
    }

    fn get_chunk_dropped_trace(chunk: &Self::Chunk, storage: &Self::Storage) -> bool {
        todo!()
    }

    fn get_chunk_trace_id(chunk: &Self::Chunk, storage: &Self::Storage) -> u128 {
        if let Some(span) = chunk.first() {
            let tid = span.get_meta("_dd.p.tid", storage).and_then(|v| u64::from_str_radix(v.borrow(), 16).ok()).unwrap_or(0);
            tid as u128 | span.trace_id as u128
        } else {
            0
        }
    }

    fn get_chunk_sampling_mechanism(chunk: &Self::Chunk, storage: &Self::Storage) -> u32 {
        todo!()
    }

    fn set_chunk_priority(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: i32) {
        todo!()
    }

    fn set_chunk_origin(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: D::Text) {
        todo!()
    }

    fn set_chunk_dropped_trace(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: bool) {
        todo!()
    }

    fn set_chunk_trace_id(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u128) {
        for span in chunk.iter_mut() {
            span.set_trace_id(value, storage);
        }
    }

    fn set_chunk_sampling_mechanism(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u32) {
        todo!()
    }

    fn get_span_service<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.service.get(storage)
    }

    fn get_span_name<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.name.get(storage)
    }

    fn get_span_resource<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.resource.get(storage)
    }

    fn get_span_type<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.r#type.get(storage)
    }

    fn get_span_span_id(span: &Self::Span, _storage: &Self::Storage) -> u64 {
        span.span_id
    }

    fn get_span_parent_id(span: &Self::Span, _storage: &Self::Storage) -> u64 {
        span.parent_id
    }

    fn get_span_start(span: &Self::Span, _storage: &Self::Storage) -> i64 {
        span.start
    }

    fn get_span_duration(span: &Self::Span, _storage: &Self::Storage) -> i64 {
        span.duration
    }

    fn get_span_error(span: &Self::Span, _storage: &Self::Storage) -> bool {
        span.error != 0
    }

    fn get_span_env<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.get_meta("env", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_version<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.get_meta("version", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_component<'a>(span: &'a Self::Span, storage: &'a Self::Storage) -> &'a D::Text {
        span.get_meta("component", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_kind(span: &Self::Span, storage: &Self::Storage) -> SpanKind {
        let kind = span.get_meta("kind", storage).map(|v| v.borrow()).unwrap_or("");
        parse_span_kind(kind)
    }

    fn set_span_service(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.service.set(storage, value)
    }

    fn set_span_name(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.name.set(storage, value)
    }

    fn set_span_resource(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.resource.set(storage, value)
    }

    fn set_span_type(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.r#type.set(storage, value)
    }

    fn set_span_span_id(span: &mut Self::Span, _storage: &mut Self::Storage, value: u64) {
        span.span_id = value;
    }

    fn set_span_parent_id(span: &mut Self::Span, _storage: &mut Self::Storage, value: u64) {
        span.parent_id = value;
    }

    fn set_span_start(span: &mut Self::Span, _storage: &mut Self::Storage, value: i64) {
        span.start = value;
    }

    fn set_span_duration(span: &mut Self::Span, _storage: &mut Self::Storage, value: i64) {
        span.duration = value;
    }

    fn set_span_error(span: &mut Self::Span, _storage: &mut Self::Storage, value: bool) {
        span.error = value as i32;
    }

    fn set_span_env(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.set_meta("env", storage, value);
    }

    fn set_span_version(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.set_meta("version", storage, value);
    }

    fn set_span_component(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text) {
        span.set_meta("component", storage, value);
    }

    fn set_span_kind(span: &mut Self::Span, storage: &mut Self::Storage, value: SpanKind) {
        match span_kind_to_str(value) {
            Some(kind) => { span.set_meta("kind", storage, kind); },
            None => span.remove_meta("kind", storage),
        }
    }

    fn get_link_trace_id(_link: &Self::SpanLink, _storage: &Self::Storage) -> u128 {
        0
    }

    fn get_link_span_id(_link: &Self::SpanLink, _storage: &Self::Storage) -> u64 {
        0
    }

    fn get_link_trace_state<'a>(_link: &'a Self::SpanLink, _storage: &'a Self::Storage) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_link_flags(_link: &Self::SpanLink, _storage: &Self::Storage) -> u32 {
        0
    }

    fn set_link_trace_id(_link: &mut Self::SpanLink, _storage: &mut Self::Storage, _value: u128) {
    }

    fn set_link_span_id(_link: &mut Self::SpanLink, _storage: &mut Self::Storage, _value: u64) {
    }

    fn set_link_trace_state(_link: &mut Self::SpanLink, _storage: &mut Self::Storage, _value: D::Text) {
    }

    fn set_link_flags(_link: &mut Self::SpanLink, _storage: &mut Self::Storage, _value: u32) {
    }

    fn get_event_time_unix_nano(_event: &Self::SpanEvent, _storage: &Self::Storage) -> u64 {
        0
    }

    fn get_event_name<'a>(_event: &'a Self::SpanEvent, _storage: &'a Self::Storage) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn set_event_time_unix_nano(_event: &mut Self::SpanEvent, _storage: &mut Self::Storage, _value: u64) {
    }

    fn set_event_name(_event: &mut Self::SpanEvent, _storage: &mut Self::Storage, _value: D::Text) {
    }
}
//note: trait bound `trace::TraceAttributes<'_, T, D, trace::AttrRef<'_, <T as trace::TraceProjector<D>>::Span>, <T as trace::TraceProjector<D>>::Span, 0>: trace::TraceAttributesOp<'_, T, D, <T as trace::TraceProjector<D>>::Span>` was not satisfied
impl<'a, D: TraceData, const Mut: u8> TraceAttributesOp<'a, ChunkCollection<D>, D, Span> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, Span>, Span, Mut> {
    type Array = ();
    type Map = ();

    fn get(container: &'a Span, storage: &Storage<D>, key: D::Text) -> Option<AttributeAnyGetterContainer<'a, Self, ChunkCollection<D>, D, Span>> {
        storage.find(key).and_then(|r| {
            if let Some(meta) = container.meta.get(&r) {
                Some(AttributeAnyContainer::String(meta.get(storage)))
            } else if let Some(metric) = container.metrics.get(&r) {
                Some(AttributeAnyContainer::Double(*metric))
            } else {
                None
            }
        })
    }
}

impl<'a, D: TraceData> TraceAttributesMutOp<'a, ChunkCollection<D>, D, Span> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'a, Span>, Span> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = ();
    type MutBoolean = &'a mut f64;
    type MutInteger = &'a mut f64;
    type MutDouble = &'a mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut(container: &mut Span, storage: &mut Storage<D>, key: D::Text) -> Option<AttributeAnySetterContainer<'a, Self, ChunkCollection<D>, D, Span>> {
        storage.find(key).and_then(|r| {
            if let Some(meta) = container.meta.get_mut(&r) {
                Some(AttributeAnyContainer::String(meta))
            } else if let Some(metric) = container.metrics.get_mut(&r) {
                Some(AttributeAnyContainer::Double(metric))
            } else {
                None
            }
        })
    }

    fn set(container: &mut Span, storage: &mut Storage<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'a, Self, ChunkCollection<D>, D, Span> {
        match value {
            AttributeAnyValueType::String => AttributeAnyContainer::String(container.set_meta(key, storage, "")),
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(container.set_metric(key, storage, 0.)),
            AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(container.set_metric(key, storage, 0.)),
            AttributeAnyValueType::Double => AttributeAnyContainer::Double(container.set_metric(key, storage, 0.)),
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }

    fn remove(container: &mut Span, storage: &mut Storage<D>, key: D::Text) {
        container.remove_meta(key.clone(), storage);
        container.remove_metric(key, storage);
    }
}

impl<'a, D: TraceData> TraceAttributesString<ChunkCollection<D>, D> for &'a mut TraceStringRef {
    fn get(&self, storage: &Storage<D>) -> &D::Text {
        (**self).get(storage)
    }

    fn set(self, storage: &mut Storage<D>, value: D::Text) {
        self.set(storage, value)
    }
}

impl<'a> TraceAttributesBoolean for &'a mut f64 {
    fn get(&self) -> bool {
        self.total_cmp(&0.) == Ordering::Equal
    }

    fn set(self, value: bool) {
        *self = value as i32 as f64;
    }
}

impl<'a> TraceAttributesInteger for &'a mut f64 {
    fn get(&self) -> i64 {
        **self as i64
    }

    fn set(self, value: i64) {
        *self = value as f64;
    }
}

impl<'a> TraceAttributesDouble for &'a mut f64 {
    fn get(&self) -> f64 {
        **self
    }

    fn set(self, value: f64) {
        *self = value;
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

/*
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
*/