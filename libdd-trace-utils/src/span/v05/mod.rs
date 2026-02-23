// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::cmp::Ordering;
use crate::span::{OwnedTraceData, TraceProjector, Traces, TraceAttributes, TraceAttributesOp, parse_span_kind, span_kind_to_str, AttributeAnyContainer, TraceAttributesMutOp, TraceAttributesMut, AttributeAnyValueType, TraceAttributesString, AttributeAnySetterContainer, AttributeAnyGetterContainer, TraceAttributesBoolean, TraceAttributesInteger, TraceAttributesDouble, SpanDataContents, AttrRef, TraceData, IntoData, TraceDataLifetime, TracesMut, TraceAttributeGetterTypes, TraceAttributeSetterTypes};

use anyhow::Result;
use serde::Serialize;
use std::borrow::Borrow;
use std::hash::Hash;
use std::slice::Iter;
use hashbrown::{Equivalent, HashMap};
use hashbrown::hash_map::Entry;
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
        K: ?Sized + Hash + Equivalent<D::Text>,
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
        K: ?Sized + Hash + Equivalent<D::Text>,
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
        K: ?Sized + Hash + Equivalent<D::Text>,
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
        K: ?Sized + Hash + Equivalent<D::Text>,
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


pub struct ChunkCollection<D: TraceData> {
    pub dict: Storage<D>,
    pub chunks: Vec<Vec<Span>>,
    // TODO: collect header data here
}

/// Sets `key` → `value` in the meta of the first span of every chunk.
///
/// The value is interned once; subsequent chunks reuse the same storage entry
/// via reference-count increments, avoiding any cloning of `D::Text`.
fn set_first_span_meta_all_chunks<D: OwnedTraceData>(trace: &mut Trace, storage: &mut Storage<D>, key: &'static str, value: D::Text) {
    let key_ref = storage.add(key);
    let value_ref = storage.add(value);
    for chunk in trace.iter_mut() {
        if let Some(span) = chunk.first_mut() {
            storage.incref(value_ref);
            match span.meta.entry(key_ref) {
                Entry::Occupied(mut e) => {
                    storage.decref(e.insert(value_ref));
                }
                Entry::Vacant(e) => {
                    storage.incref(key_ref);
                    e.insert(value_ref);
                }
            }
        }
    }
    storage.decref(key_ref);
    storage.decref(value_ref);
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

impl<'s, D: TraceDataLifetime<'s>> TraceProjector<'s, D> for ChunkCollection<D> where D: 's {
    type Storage = Storage<D>;
    type Trace = Trace;
    type Chunk = Chunk;
    type Span = Span;
    type SpanLink = SpanLink;
    type SpanEvent = SpanEvent;

    fn project(&'s self) -> Traces<'s, Self, D> {
        Traces::new(&self.chunks, &self.dict)
    }

    fn project_mut(&'s mut self) -> TracesMut<'s, Self, D> {
        Traces::new_mut(&mut self.chunks, &mut self.dict)
    }

    fn add_chunk<'b>(trace: &'b mut Trace, _storage: &mut Storage<D>) -> &'b mut Chunk {
        trace.push(Vec::new());
        unsafe { trace.last_mut().unwrap_unchecked() }
    }

    fn chunk_iterator(trace: &'s Trace) -> Iter<'s, Vec<Span>> {
        trace.iter()
    }

    fn retain_chunks<'b, F: for<'c> FnMut(&'c mut Chunk, &'c mut Storage<D>) -> bool>(trace: &'b mut Trace, storage: &'b mut Storage<D>, mut predicate: F) {
        trace.retain_mut(move |chunk| {
            if predicate(chunk, storage) {
                true
            } else {
                free_chunk_data(chunk, storage);
                false
            }
        })
    }

    fn retain_spans<'b, F: FnMut(&mut Span, &mut Storage<D>) -> bool>(chunk: &'b mut Chunk, storage: &'b mut Storage<D>, mut predicate: F) {
        chunk.retain_mut(|span| {
            if predicate(span, storage) {
                true
            } else {
                free_span_data(span, storage);
                false
            }
        })
    }

    fn add_span<'b>(chunk: &'b mut Chunk, storage: &mut Storage<D>) -> &'b mut Span {
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

    fn span_iterator(chunk: &'s Chunk) -> Iter<'s, Span> {
        chunk.iter()
    }

    fn add_span_link<'b>(_span: &'b mut Span, _storage: &mut Storage<D>) -> &'b mut SpanLink {
        &mut []
    }

    fn span_link_iterator(_span: &'s Span) -> Iter<'s, SpanLink> {
        [].iter()
    }

    fn retain_span_links<'b, F: FnMut(&mut SpanLink, &mut Storage<D>) -> bool>(_span: &'b mut Span, _storage: &'b mut Storage<D>, _predicate: F) {
    }

    fn add_span_event<'b>(_span: &'b mut Span, _storage: &mut Storage<D>) -> &'b mut SpanEvent {
        &mut []
    }

    fn span_event_iterator(_span: &'s Span) -> Iter<'s, SpanEvent> {
        [].iter()
    }

    fn retain_span_events<'b, F: FnMut(&mut SpanEvent, &mut Storage<D>) -> bool>(_span: &'b mut Span, _storage: &'b mut Storage<D>, _predicate: F) {
    }

    fn get_trace_container_id(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("container_id", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_language_name(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("language", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_language_version(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("language_version", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_tracer_version(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("tracer_version", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_runtime_id(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("runtime-id", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_env(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("env", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_hostname(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("_dd.hostname", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn get_trace_app_version(trace: &Trace, storage: &'s Storage<D>) -> &'s D::Text {
        trace.first()
            .and_then(|chunk| chunk.first())
            .and_then(|span| span.get_meta("version", storage))
            .unwrap_or(D::Text::default_ref())
    }

    fn set_trace_container_id(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "container_id", value);
    }

    fn set_trace_language_name(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "language", value);
    }

    fn set_trace_language_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "language_version", value);
    }

    fn set_trace_tracer_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "tracer_version", value);
    }

    fn set_trace_runtime_id(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "runtime-id", value);
    }

    fn set_trace_env(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "env", value);
    }

    fn set_trace_hostname(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "_dd.hostname", value);
    }

    fn set_trace_app_version(trace: &mut Trace, storage: &mut Storage<D>, value: D::Text) where D: OwnedTraceData {
        set_first_span_meta_all_chunks(trace, storage, "version", value);
    }

    fn get_chunk_priority<'a>(chunk: &'a Chunk, storage: &'a Storage<D>) -> i32 {
        chunk.first()
            .and_then(|span| span.get_metric("_sampling_priority_v1", storage))
            .unwrap_or(1.0) as i32
    }

    fn get_chunk_origin(chunk: &'s Chunk, storage: &'s Storage<D>) -> &'s D::Text {
        if let Some(span) = chunk.first() {
            span.get_meta("_dd.origin", storage)
                .unwrap_or_else(|| storage.get(span.service))
        } else {
            D::Text::default_ref()
        }
    }

    fn get_chunk_dropped_trace<'a>(_chunk: &'a Chunk, _storage: &'a Storage<D>) -> bool {
        false
    }

    fn get_chunk_trace_id<'a>(chunk: &'a Chunk, storage: &'a Storage<D>) -> u128 {
        if let Some(span) = chunk.first() {
            let tid = span.get_meta("_dd.p.tid", storage).and_then(|v| u64::from_str_radix(v.borrow(), 16).ok()).unwrap_or(0);
            tid as u128 | span.trace_id as u128
        } else {
            0
        }
    }

    fn get_chunk_sampling_mechanism<'a>(chunk: &'a Chunk, storage: &'a Storage<D>) -> u32 {
        chunk.first()
            .and_then(|span| span.get_metric("_dd.span_sampling.mechanism", storage))
            .unwrap_or(0.0) as u32
    }

    fn set_chunk_priority(chunk: &mut Chunk, storage: &mut Storage<D>, value: i32) {
        if let Some(span) = chunk.first_mut() {
            span.set_metric("_sampling_priority_v1", storage, value as f64);
        }
    }

    fn set_chunk_origin(chunk: &mut Chunk, storage: &mut Storage<D>, value: D::Text) {
        if let Some(span) = chunk.first_mut() {
            span.set_meta("_dd.origin", storage, value);
        }
    }

    fn set_chunk_dropped_trace(_chunk: &mut Chunk, _storage: &mut Storage<D>, _value: bool) {
    }

    fn set_chunk_trace_id(chunk: &mut Chunk, storage: &mut Storage<D>, value: u128) where D: OwnedTraceData {
        for span in chunk.iter_mut() {
            span.set_trace_id(value, storage);
        }
    }

    fn set_chunk_sampling_mechanism(chunk: &mut Chunk, storage: &mut Storage<D>, value: u32) {
        if let Some(span) = chunk.first_mut() {
            span.set_metric("_dd.span_sampling.mechanism", storage, value as f64);
        }
    }

    fn get_span_service<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
        storage.get(span.service)
    }

    fn get_span_name<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
        storage.get(span.name)
    }

    fn get_span_resource<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
        storage.get(span.resource)
    }

    fn get_span_type<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
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

    fn get_span_env<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
        span.get_meta("env", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_version<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
        span.get_meta("version", storage).unwrap_or(D::Text::default_ref())
    }

    fn get_span_component<'a>(span: &Span, storage: &'s Storage<D>) -> &'s D::Text {
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

    fn get_link_trace_id(_link: &'s SpanLink, _storage: &'s Storage<D>) -> u128 {
        0
    }

    fn get_link_span_id(_link: &'s SpanLink, _storage: &'s Storage<D>) -> u64 {
        0
    }

    fn get_link_trace_state(_link: &'s SpanLink, _storage: &'s Storage<D>) -> &'s D::Text {
        D::Text::default_ref()
    }

    fn get_link_flags(_link: &'s SpanLink, _storage: &'s Storage<D>) -> u32 {
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

    fn get_event_time_unix_nano(_event: &'s SpanEvent, _storage: &'s Storage<D>) -> u64 {
        0
    }

    fn get_event_name(_event: &'s SpanEvent, _storage: &'s Storage<D>) -> &'s D::Text {
        D::Text::default_ref()
    }

    fn set_event_time_unix_nano(_event: &mut SpanEvent, _storage: &mut Storage<D>, _value: u64) {
    }

    fn set_event_name(_event: &mut SpanEvent, _storage: &mut Storage<D>, _value: D::Text) {
    }
}
// Format-specific attribute type macros for v05
// In v05: 'a = storage lifetime, 'b = container lifetime
macro_rules! impl_v05_span_attribute_types {
    ($C:ty) => {
        impl<'a, 'b, D: TraceData, const ISMUT: u8> TraceAttributeGetterTypes<'b, 'a, ChunkCollection<D>, D, $C>
        for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, $C>, $C, ISMUT> {
            type Array = ();
            type Map = ();
        }
        impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, ChunkCollection<D>, D, $C>
        for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, $C>, $C> {
            type MutString = &'a mut TraceStringRef;
            type MutBytes = ();
            type MutBoolean = &'b mut f64;
            type MutInteger = &'b mut f64;
            type MutDouble = &'b mut f64;
            type MutArray = ();
            type MutMap = ();
        }
    };
}

macro_rules! impl_v05_null_attribute_types {
    ($C:ty) => {
        impl<'a, 'b, D: TraceData, const ISMUT: u8> TraceAttributeGetterTypes<'b, 'a, ChunkCollection<D>, D, $C>
        for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, $C>, $C, ISMUT> {
            type Array = ();
            type Map = ();
        }
        impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, ChunkCollection<D>, D, $C>
        for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, $C>, $C> {
            type MutString = ();
            type MutBytes = ();
            type MutBoolean = ();
            type MutInteger = ();
            type MutDouble = ();
            type MutArray = ();
            type MutMap = ();
        }
    };
}

//note: trait bound `trace::TraceAttributes<'_, T, D, trace::AttrRef<'_, <T as trace::TraceProjector<D>>::Span>, <T as trace::TraceProjector<D>>::Span, 0>: trace::TraceAttributesOp<'_, T, D, <T as trace::TraceProjector<D>>::Span>` was not satisfied
impl_v05_span_attribute_types!(Span);

impl<'a, 'b, D: TraceData, const ISMUT: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Span> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span, ISMUT> {
    fn get<K>(container: &'b Span, storage: &'a Storage<D>, key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
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

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Span> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span> {
    fn get_mut<K>(container: &'b mut Span, storage: &mut Storage<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>,
    {
        let r = storage.find(key)?;
        if let Some(meta) = container.meta.get_mut(&r) {
            // SAFETY: In v05, TraceStringRef indices are stable for the storage's lifetime 'a.
            // We transmute from 'b (container lifetime) to 'a (storage lifetime).
            // This is sound because the actual data lives in storage with lifetime 'a.
            let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
            Some(AttributeAnyContainer::String(meta_storage))
        } else if let Some(metric) = container.metrics.get_mut(&r) {
            Some(AttributeAnyContainer::Double(metric))
        } else {
            None
        }
    }

    fn set(container: &'b mut Span, storage: &mut Storage<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Span> {
        match value {
            AttributeAnyValueType::String => {
                let meta = container.set_meta(key, storage, "");
                // SAFETY: transmute from 'b to 'a (same reasoning as get_mut)
                let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
                AttributeAnyContainer::String(meta_storage)
            },
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
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        container.remove_meta(key, storage);
        container.remove_metric(key, storage);
    }
}

// This impl works for &'storage mut TraceStringRef when both lifetimes are 'storage
impl<'storage, D: TraceDataLifetime<'storage> + 'storage> TraceAttributesString<'storage, 'storage, ChunkCollection<D>, D> for &'storage mut TraceStringRef {
    fn get(&self, storage: &'storage Storage<D>) -> &'storage D::Text {
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
impl_v05_null_attribute_types!([(); 0]);

impl<'b, 'a, D: TraceData, const ISMUT: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, [(); 0]>, [(); 0], ISMUT> {
    fn get<K>(_container: &'b [(); 0], _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>,
    {
        None
    }
}

impl<'b, 'a, D: TraceData> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, [(); 0]>, [(); 0]> {
    fn get_mut<K>(_container: &'b mut [(); 0], _storage: &mut Storage<D>, _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        None
    }

    fn set(_container: &'b mut [(); 0], _storage: &mut Storage<D>, _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut [(); 0], _storage: &mut Storage<D>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
    }
}

impl<'a, 'b, D: TraceData, const ISMUT: u8> TraceAttributeGetterTypes<'b, 'a, ChunkCollection<D>, D, [(); 0]>
for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span, ISMUT> {
    type Array = ();
    type Map = ();
}

impl<'b, 'a, D: TraceData, const ISMUT: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, [(); 0]> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Span>, Span, ISMUT> {
    fn get<K>(_container: &'b [(); 0], _storage: &'a Storage<D>, _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, [(); 0]>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        None
    }
}

impl_v05_span_attribute_types!(Trace);

impl<'b, 'a, D: TraceData, const ISMUT: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Trace> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Trace>, Trace, ISMUT> {
    fn get<K>(container: &'b Trace, storage: &'a Storage<D>, key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let span = container.first()?.first()?;
        storage.find(key).and_then(move |r| {
            if let Some(meta) = span.meta.get(&r) {
                Some(AttributeAnyContainer::String(storage.get(*meta)))
            } else if let Some(metric) = span.metrics.get(&r) {
                Some(AttributeAnyContainer::Double(*metric))
            } else {
                None
            }
        })
    }
}

impl_v05_span_attribute_types!(Chunk);

impl<'b, 'a, D: TraceData, const ISMUT: u8> TraceAttributesOp<'b, 'a, ChunkCollection<D>, D, Chunk> for TraceAttributes<'a, ChunkCollection<D>, D, AttrRef<'b, Chunk>, Chunk, ISMUT> {
    fn get<K>(container: &'b Chunk, storage: &'a Storage<D>, key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let span = container.first()?;
        storage.find(key).and_then(move |r| {
            if let Some(meta) = span.meta.get(&r) {
                Some(AttributeAnyContainer::String(storage.get(*meta)))
            } else if let Some(metric) = span.metrics.get(&r) {
                Some(AttributeAnyContainer::Double(*metric))
            } else {
                None
            }
        })
    }
}

impl<'b, 'a, D: TraceData> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Chunk> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Chunk>, Chunk> {
    fn get_mut<K>(container: &'b mut Chunk, storage: &mut Storage<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let span = container.first_mut()?;
        let r = storage.find(key)?;
        if let Some(meta) = span.meta.get_mut(&r) {
            // SAFETY: same reasoning as Span::get_mut — transmute 'b→'a
            let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
            Some(AttributeAnyContainer::String(meta_storage))
        } else if let Some(metric) = span.metrics.get_mut(&r) {
            Some(AttributeAnyContainer::Double(metric))
        } else {
            None
        }
    }

    fn set(container: &'b mut Chunk, storage: &mut Storage<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Chunk> {
        if let Some(span) = container.first_mut() {
            match value {
                AttributeAnyValueType::String => {
                    let meta = span.set_meta(key, storage, "");
                    // SAFETY: same reasoning as Span::set — transmute 'b→'a
                    let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
                    AttributeAnyContainer::String(meta_storage)
                },
                AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
                AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Double => AttributeAnyContainer::Double(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
                AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
            }
        } else {
            AttributeAnyContainer::Map(())
        }
    }

    fn remove<K>(container: &mut Chunk, storage: &mut Storage<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(span) = container.first_mut() {
            span.remove_meta(key, storage);
            span.remove_metric(key, storage);
        }
    }
}

impl<'b, 'a, D: TraceData> TraceAttributesMutOp<'b, 'a, ChunkCollection<D>, D, Trace> for TraceAttributesMut<'a, ChunkCollection<D>, D, AttrRef<'b, Trace>, Trace> {
    fn get_mut<K>(container: &'b mut Trace, storage: &mut Storage<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let span = container.first_mut()?.first_mut()?;
        let r = storage.find(key)?;
        if let Some(meta) = span.meta.get_mut(&r) {
            // SAFETY: same reasoning as Span::get_mut — transmute 'b→'a
            let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
            Some(AttributeAnyContainer::String(meta_storage))
        } else if let Some(metric) = span.metrics.get_mut(&r) {
            Some(AttributeAnyContainer::Double(metric))
        } else {
            None
        }
    }

    fn set(container: &'b mut Trace, storage: &mut Storage<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, ChunkCollection<D>, D, Trace> {
        // set operates on the first span of the first chunk only (must return a stable reference)
        if let Some(span) = container.first_mut().and_then(|c| c.first_mut()) {
            match value {
                AttributeAnyValueType::String => {
                    let meta = span.set_meta(key, storage, "");
                    // SAFETY: same reasoning as Span::set — transmute 'b→'a
                    let meta_storage: &'a mut TraceStringRef = unsafe { std::mem::transmute(meta) };
                    AttributeAnyContainer::String(meta_storage)
                },
                AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
                AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Double => AttributeAnyContainer::Double(span.set_metric(key, storage, 0.)),
                AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
                AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
            }
        } else {
            AttributeAnyContainer::Map(())
        }
    }

    fn remove<K>(container: &mut Trace, storage: &mut Storage<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        // remove operates on the first span of ALL chunks (no reference returned)
        for chunk in container.iter_mut() {
            if let Some(span) = chunk.first_mut() {
                span.remove_meta(key, storage);
                span.remove_metric(key, storage);
            }
        }
    }
}







pub fn from_v04_span<T: TraceData>(
    span: crate::span::v04::Span<T>,
    dict: &mut Storage<T>,
) -> Result<Span> {
    let meta_len = span.meta.len();
    let metrics_len = span.metrics.len();
    Ok(Span {
        service: dict.add(span.service),
        name: dict.add(span.name),
        resource: dict.add(span.resource),
        trace_id: span.trace_id as u64,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span.meta.into_iter().fold(
            HashMap::with_capacity(meta_len),
            |mut meta, (k, v)| {
                meta.insert(dict.add(k), dict.add(v));
                meta
            },
        ),
        metrics: span.metrics.into_iter().fold(
            HashMap::with_capacity(metrics_len),
            |mut metrics, (k, v)| {
                metrics.insert(dict.add(k), v);
                metrics
            },
        ),
        r#type: dict.add(span.r#type),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::SpanBytes;
    use crate::span::BytesData;
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

        let mut dict = Storage::<BytesData>::default();
        let v05_span = from_v04_span(span, &mut dict).unwrap();

        let get_ref = |s: &str| -> TraceStringRef {
            dict.find(&BytesString::from(s)).unwrap()
        };

        assert_eq!(v05_span.service, get_ref("service"));
        assert_eq!(v05_span.name, get_ref("name"));
        assert_eq!(v05_span.resource, get_ref("resource"));
        assert_eq!(v05_span.r#type, get_ref("type"));
        assert_eq!(v05_span.trace_id, 1);
        assert_eq!(v05_span.span_id, 1);
        assert_eq!(v05_span.parent_id, 0);
        assert_eq!(v05_span.start, 1);
        assert_eq!(v05_span.duration, 111);
        assert_eq!(v05_span.error, 0);
        assert_eq!(v05_span.meta.len(), 1);
        assert_eq!(v05_span.metrics.len(), 1);

        assert_eq!(
            *v05_span.meta.get(&get_ref("meta_field")).unwrap(),
            get_ref("meta_value")
        );
        assert_eq!(
            *v05_span.metrics.get(&get_ref("metrics_field")).unwrap(),
            1.1
        );
    }
}