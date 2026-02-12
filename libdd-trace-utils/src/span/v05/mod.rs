// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use crate::span::{v05::dict::SharedDict, OwnedTraceData, TraceProjector, Traces, TraceAttributes, TraceAttributesOp, parse_span_kind, span_kind_to_str, AttributeAnyContainer, TraceAttributesMutOp, TraceAttributesMut, AttributeAnyValueType, TraceAttributesString, TraceAttributesBytes, AttributeAnySetterContainer, AttributeAnyGetterContainer, TraceAttributesBoolean, TraceAttributesInteger, TraceAttributesDouble, SpanBytes, SpanDataContents, SpanText, AttrRef, TraceData, IntoData, TraceDataLifetime, TracesMut};
use anyhow::Result;
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::slice::Iter;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
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
    fn set_trace_id<D: OwnedTraceData>(&mut self, trace_id: u128, storage: &mut Storage<D>) {
        self.trace_id = trace_id as u64;
        if trace_id >> 64 > 0 {
            self.set_meta("_dd.p.tid", storage, IntoData::<D::Text>::into(format!("{:016x}", (trace_id >> 64) as u64)));
        } else {
            self.remove_meta("_dd.p.tid", storage);
        }
    }

    fn get_meta<'a, D: TraceData, K>(&self, key: &K, storage: &'a Storage<D>) -> Option<&'a D::Text>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        storage.find(key).and_then(|r| self.meta.get(&r)).map(|r| storage.get(*r))
    }

    fn set_meta<D: TraceData, K: IntoData<D::Text>, V: IntoData<D::Text>>(&mut self, key: K, storage: &mut Storage<D>, value: V) -> &mut TraceStringRef {
        let r = storage.add(key);
        let value = storage.add(value);
        match self.meta.entry(r) {
            Entry::Occupied(mut e) => {
                storage.decref(r);
                storage.decref(e.insert(value));
                e.into_mut()
            }
            Entry::Vacant(e) => e.insert(value),
        }
    }

    fn remove_meta<D: TraceData, K>(&mut self, key: &K, storage: &mut Storage<D>)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        if let Some(r) = storage.find(key) {
            if let Some(removed) = self.meta.remove(&r) {
                storage.decref(r);
                storage.decref(removed);
            }
        }
    }

    fn get_metric<D: TraceData, K>(&self, key: &K, storage: &Storage<D>) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        storage.find(key).and_then(|r| self.metrics.get(&r).map(|v| *v))
    }

    fn set_metric<D: TraceData, K: IntoData<D::Text>>(&mut self, key: K, storage: &mut Storage<D>, value: f64) -> &mut f64 {
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

    fn remove_metric<D: TraceData, K>(&mut self, key: &K, storage: &mut Storage<D>)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        if let Some(r) = storage.find(key) {
            if self.meta.remove(&r).is_some() {
                storage.decref(r);
            }
        }
    }
}

type Trace = Vec<Vec<Span>>;
type Chunk = Vec<Span>;
type Storage<D> = StaticDataVec<D, TraceDataText>;
type SpanLink = [(); 0];
type SpanEvent = [(); 0];


pub struct ChunkCollection<D: TraceData + 'static> {
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

impl<D: TraceData + 'static> TraceProjector<D> for ChunkCollection<D> {
    type Storage<'a> = Storage<D>;
    type Trace<'a> = Trace;
    type Chunk<'a> = Chunk;
    type Span<'a> = Span;
    type SpanLink<'a> = SpanLink;
    type SpanEvent<'a> = SpanEvent;
/*
    type AttributeTrace<'a> = TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, Trace>, Trace>;
    type AttributeChunk<'a> = TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, Chunk>, Chunk>;
    type AttributeSpan<'a> = TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, Span>, Span>;
    type AttributeSpanLink<'a> = TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, SpanLink>, SpanLink>;
    type AttributeSpanEvent<'a> = TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'a, SpanEvent>, SpanEvent>;
*/
    fn project<'a>(&'a self) -> Traces<Self, D> where D: TraceDataLifetime<'a> {
        Traces::new(&self.chunks, &self.dict)
    }

    fn project_mut<'a>(&'a mut self) -> TracesMut<Self, D> where D: TraceDataLifetime<'a> {
        Traces::new_mut(&mut self.chunks, &mut self.dict)
    }

    fn add_chunk<'a>(trace: &'a mut Trace, _storage: &mut Storage<D>) -> &'a mut Chunk {
        trace.push(Vec::new());
        unsafe { trace.last_mut().unwrap_unchecked() }
    }

    fn chunk_iterator<'a>(trace: &'a Trace) -> Iter<'a, Vec<Span>> {
        trace.iter()
    }

    fn retain_chunks<'b, 'a, F: for<'c> FnMut(&'c mut Self::Chunk<'c>, &'c mut Self::Storage<'a>) -> bool>(trace: &'b mut Self::Trace<'b>, storage: &'a mut Self::Storage<'a>, mut predicate: F) {
        trace.retain_mut(move |chunk| {
            if predicate(chunk, storage) {
                true
            } else {
                free_chunk_data(chunk, storage);
                false
            }
        })
    }

    fn retain_spans<'b, 'a, F: for<'c> FnMut(&'c mut Self::Span<'c>, &'c mut Self::Storage<'a>) -> bool>(chunk: &'b mut Self::Chunk<'b>, storage: &'a mut Self::Storage<'a>, mut predicate: F) {
        chunk.retain_mut(|span| {
            if predicate(span, storage) {
                true
            } else {
                free_span_data(span, storage);
                false
            }
        })
    }

    fn add_span<'a>(chunk: &'a mut Chunk, storage: &mut Storage<D>) -> &'a mut Span {
        chunk.push(Span::default());
        let (trace_id, tidkey) = if let Some(first_span) = chunk.first() {
            if let Some(key) = storage.find("_dd.p.tid") {
                (first_span.trace_id, first_span.meta.get(&key).cloned().map(|tid| (tid, key)))
            } else {
                (first_span.trace_id, None)
            }
        } else {
            (0, None)
        };
        let span = unsafe { chunk.last_mut().unwrap_unchecked() };
        span.trace_id = trace_id;
        if let Some((tid, key)) = tidkey {
            storage.incref(key);
            storage.incref(tid);
            span.meta.insert(key, tid);
        }
        span
    }

    fn span_iterator<'a>(chunk: &'a Chunk) -> Iter<'a, Span> {
        chunk.iter()
    }

    fn add_span_link<'a>(_span: &'a mut Span, _storage: &mut Storage<D>) -> &'a mut SpanLink {
        &mut []
    }

    fn span_link_iterator<'a>(_span: &'a Span) -> Iter<'a, SpanLink> {
        [].iter()
    }

    fn retain_span_links<'b, 'a, F: for<'c> FnMut(&'c mut Self::SpanLink<'c>, &'c mut Self::Storage<'a>) -> bool>(span: &'b mut Self::Span<'b>, storage: &'a mut Self::Storage<'a>, predicate: F) {
    }

    fn add_span_event<'a>(_span: &mut Span, _storage: &mut Storage<D>) -> &'a mut SpanEvent {
        &mut []
    }

    fn span_event_iterator<'a>(_span: &'a Span) -> Iter<'a, SpanEvent> {
        [].iter()
    }

    fn retain_span_events<'b, 'a, F: for<'c> FnMut(&'c mut Self::SpanEvent<'c>, &'c mut Self::Storage<'a>) -> bool>(span: &'b mut Self::Span<'b>, storage: &'a mut Self::Storage<'a>, predicate: F) {
    }

    fn get_trace_container_id<'a>(_trace: &Trace, _storage: &'a Storage<D>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_language_name<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_language_version<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_tracer_version<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_runtime_id<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_env<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_hostname<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_trace_app_version<'a>(trace: &Trace, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn set_trace_container_id(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_language_name(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_language_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_tracer_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_runtime_id(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_env(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_hostname(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_trace_app_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn get_chunk_priority(chunk: &Chunk, storage: &Storage<D>) -> i32 {
        todo!()
    }

    fn get_chunk_origin<'a>(chunk: &Chunk, storage: &'a Storage<D>) -> &'a D::Text {
        todo!()
    }

    fn get_chunk_dropped_trace(chunk: &Chunk, storage: &Storage<D>) -> bool {
        todo!()
    }

    fn get_chunk_trace_id(chunk: &Chunk, storage: &Storage<D>) -> u128 {
        if let Some(span) = chunk.first() {
            let tid = span.get_meta("_dd.p.tid", storage).and_then(|v| u64::from_str_radix(v.borrow(), 16).ok()).unwrap_or(0);
            tid as u128 | span.trace_id as u128
        } else {
            0
        }
    }

    fn get_chunk_sampling_mechanism(chunk: &Chunk, storage: &Storage<D>) -> u32 {
        todo!()
    }

    fn set_chunk_priority(chunk: &mut Chunk, storage: &mut Storage<D>, value: i32) {
        todo!()
    }

    fn set_chunk_origin(chunk: &mut Chunk, storage: &mut Storage<D>, value: D::Text) {
        todo!()
    }

    fn set_chunk_dropped_trace(chunk: &mut Chunk, storage: &mut Storage<D>, value: bool) {
        todo!()
    }

    fn set_chunk_trace_id(chunk: &mut Chunk, storage: &mut Storage<D>, value: u128) where D: OwnedTraceData {
        for span in chunk.iter_mut() {
            span.set_trace_id(value, storage);
        }
    }

    fn set_chunk_sampling_mechanism(chunk: &mut Chunk, storage: &mut Storage<D>, value: u32) {
        todo!()
    }

    fn get_span_service<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        storage.get(span.service)
    }

    fn get_span_name<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        storage.get(span.name)
    }

    fn get_span_resource<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        storage.get(span.resource)
    }

    fn get_span_type<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        storage.get(span.r#type)
    }

    fn get_span_span_id(span: &Span, _storage: &Storage<D>) -> u64 {
        span.span_id
    }

    fn get_span_parent_id(span: &Span, _storage: &Storage<D>) -> u64 {
        span.parent_id
    }

    fn get_span_start(span: &Span, _storage: &Storage<D>) -> i64 {
        span.start
    }

    fn get_span_duration(span: &Span, _storage: &Storage<D>) -> i64 {
        span.duration
    }

    fn get_span_error(span: &Span, _storage: &Storage<D>) -> bool {
        span.error != 0
    }

    fn get_span_env<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        span.get_meta("env", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_version<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        span.get_meta("version", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_component<'a>(span: &Span, storage: &'a Storage<D>) -> &'a D::Text {
        span.get_meta("component", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_kind(span: &Span, storage: &Storage<D>) -> SpanKind {
        let kind = span.get_meta("kind", storage).map(|v| v.borrow()).unwrap_or("");
        parse_span_kind(kind)
    }

    fn set_span_service(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.service.set(storage, value)
    }

    fn set_span_name(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.name.set(storage, value)
    }

    fn set_span_resource(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.resource.set(storage, value)
    }

    fn set_span_type(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.r#type.set(storage, value)
    }

    fn set_span_span_id(span: &mut Span, _storage: &mut Storage<D>, value: u64) {
        span.span_id = value;
    }

    fn set_span_parent_id(span: &mut Span, _storage: &mut Storage<D>, value: u64) {
        span.parent_id = value;
    }

    fn set_span_start(span: &mut Span, _storage: &mut Storage<D>, value: i64) {
        span.start = value;
    }

    fn set_span_duration(span: &mut Span, _storage: &mut Storage<D>, value: i64) {
        span.duration = value;
    }

    fn set_span_error(span: &mut Span, _storage: &mut Storage<D>, value: bool) {
        span.error = value as i32;
    }

    fn set_span_env(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.set_meta("env", storage, value);
    }

    fn set_span_version(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.set_meta("version", storage, value);
    }

    fn set_span_component(span: &mut Span, storage: &mut Storage<D>, value: D::Text) {
        span.set_meta("component", storage, value);
    }

    fn set_span_kind(span: &mut Span, storage: &mut Storage<D>, value: SpanKind) {
        match span_kind_to_str(value) {
            Some(kind) => { span.set_meta("kind", storage, kind); },
            None => span.remove_meta("kind", storage),
        }
    }

    fn get_link_trace_id(_link: &SpanLink, _storage: &Storage<D>) -> u128 {
        0
    }

    fn get_link_span_id(_link: &SpanLink, _storage: &Storage<D>) -> u64 {
        0
    }

    fn get_link_trace_state<'a>(_link: &SpanLink, _storage: &'a Storage<D>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_link_flags(_link: &SpanLink, _storage: &Storage<D>) -> u32 {
        0
    }

    fn set_link_trace_id(_link: &mut SpanLink, _storage: &mut Storage<D>, _value: u128) {
    }

    fn set_link_span_id(_link: &mut SpanLink, _storage: &mut Storage<D>, _value: u64) {
    }

    fn set_link_trace_state(_link: &mut SpanLink, _storage: &mut Storage<D>, _value: D::Text) {
    }

    fn set_link_flags(_link: &mut SpanLink, _storage: &mut Storage<D>, _value: u32) {
    }

    fn get_event_time_unix_nano(_event: &SpanEvent, _storage: &Storage<D>) -> u64 {
        0
    }

    fn get_event_name<'a>(_event: &SpanEvent, _storage: &'a Storage<D>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn set_event_time_unix_nano(_event: &mut SpanEvent, _storage: &mut Storage<D>, _value: u64) {
    }

    fn set_event_name(_event: &mut SpanEvent, _storage: &mut Storage<D>, _value: D::Text) {
    }
}
//note: trait bound `trace::TraceAttributes<'_, T, D, trace::AttrRef<'_, <T as trace::TraceProjector<D>>::Span>, <T as trace::TraceProjector<D>>::Span, 0>: trace::TraceAttributesOp<'_, T, D, <T as trace::TraceProjector<D>>::Span>` was not satisfied
impl<'a, 'b, D: TraceData + 'static, const Mut: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Span> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'b Span, storage: &'a Storage<D>, key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        storage.find(key).and_then(move |r| {
            if let Some(meta) = container.meta.get(&r) {
                Some(AttributeAnyContainer::String(storage.get(*meta)))
            } else if let Some(metric) = container.metrics.get(&r) {
                Some(AttributeAnyContainer::Double(*metric))
            } else {
                None
            }
        })
    }
}

impl<'a, 'b, D: TraceData + 'static> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Span> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span> {
    type MutString = &'b mut TraceStringRef;
    type MutBytes = ();
    type MutBoolean = &'b mut f64;
    type MutInteger = &'b mut f64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut Span, storage: &mut Storage<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        if let Some(meta) = container.meta.get_mut(&r) {
            Some(AttributeAnyContainer::String(meta))
        } else if let Some(metric) = container.metrics.get_mut(&r) {
            Some(AttributeAnyContainer::Double(metric))
        } else {
            None
        }
    }

    fn set(container: &'b mut Span, storage: &mut Storage<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span> {
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

    fn remove<K>(container: &mut Span, storage: &mut Storage<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        container.remove_meta(key, storage);
        container.remove_metric(key, storage);
    }
}

impl<'a, 'b, D: TraceDataLifetime<'a>> TraceAttributesString<'a, ChunkCollection<D>, D> for &'b mut TraceStringRef {
    fn get(&self, storage: &'a Storage<D>) -> &'a D::Text {
        storage.get(**self)
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

// Empty implementations for SpanLink and SpanEvent which don't have attributes in v05
impl<'b, 'a, D: TraceData + 'static, const Mut: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, [(); 0]>, [(); 0], Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b [(); 0], _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }
}

impl<'b, 'a, D: TraceData + 'static> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, [(); 0]>, [(); 0]> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut [(); 0], _storage: &mut Storage<D>, _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut [(); 0], _storage: &mut Storage<D>, _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut [(); 0], _storage: &mut Storage<D>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

impl<'b, 'a, D: TraceData + 'static, const Mut: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b [(); 0], _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

impl<'b, 'a, D: TraceData + 'static, const Mut: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Trace> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Trace>, Trace, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b Trace, _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

impl<'b, 'a, D: TraceData + 'static, const Mut: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Chunk> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Chunk>, Chunk, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b Chunk, _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

impl<'b, 'a, D: TraceData + 'static> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Chunk> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Chunk>, Chunk> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut Chunk, _storage: &mut Storage<D>, _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut Chunk, _storage: &mut Storage<D>, _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut Chunk, _storage: &mut Storage<D>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

impl<'b, 'a, D: TraceData + 'static> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Trace> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Trace>, Trace> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut Trace, _storage: &mut Storage<D>, _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut Trace, _storage: &mut Storage<D>, _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut Trace, _storage: &mut Storage<D>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}







pub fn from_v04_span<T: TraceData>(
    _span: crate::span::v04::Span<T>,
    _dict: &mut SharedDict<T::Text>,
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