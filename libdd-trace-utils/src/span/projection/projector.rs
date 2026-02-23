use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{OwnedTraceData, TraceDataLifetime, ImpliedPredicate};
use super::{Traces, TracesMut, TraceAttributes, TraceAttributesMut, TraceAttributesOp, TraceAttributesMutOp, AttrRef};

/// Central trait that maps a storage type to the trace data model.
///
/// Implementors provide low-level getter and setter functions for every field in the
/// trace hierarchy (trace → chunk → span → link/event) and methods for iterating and
/// mutating collections at each level. The higher-level view types ([`Traces`], [`TraceChunk`],
/// [`Span`], …) delegate to these methods.
///
/// `'s` is the lifetime of the underlying storage; all references returned by getters are
/// tied to this lifetime. `D` carries the concrete string and byte types in use.
pub trait TraceProjector<'s, D: TraceDataLifetime<'s>>: Sized + 's
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Trace>, Self::Trace>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Trace>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Chunk>, Self::Chunk>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Chunk>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Span>, Self::Span>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Span>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::SpanLink>, Self::SpanLink>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::SpanLink>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::SpanEvent>, Self::SpanEvent>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::SpanEvent>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Trace>, Self::Trace>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Trace>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Chunk>, Self::Chunk>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Chunk>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Span>, Self::Span>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Span>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::SpanLink>, Self::SpanLink>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::SpanLink>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::SpanEvent>, Self::SpanEvent>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::SpanEvent>>
{
    type Storage: 's;
    type Trace: 's;
    type Chunk: 's;
    type Span: 's;
    type SpanLink: 's;
    type SpanEvent: 's;

    fn project(&'s self) -> Traces<'s, Self, D>;
    fn project_mut(&'s mut self) -> TracesMut<'s, Self, D>;

    fn add_chunk<'b>(trace: &'b mut Self::Trace, storage: &mut Self::Storage) -> &'b mut Self::Chunk;
    fn chunk_iterator(trace: &'s Self::Trace) -> std::slice::Iter<'s, Self::Chunk>;
    fn retain_chunks<'b, F: for<'c> FnMut(&'c mut Self::Chunk, &'c mut Self::Storage) -> bool>(trace: &'b mut Self::Trace, storage: &'b mut Self::Storage, predicate: F);
    fn add_span<'b>(chunk: &'b mut Self::Chunk, storage: &mut Self::Storage) -> &'b mut Self::Span;
    fn span_iterator(chunk: &'s Self::Chunk) -> std::slice::Iter<'s, Self::Span>;
    fn retain_spans<'b, F: FnMut(&mut Self::Span, &mut Self::Storage) -> bool>(chunk: &'b mut Self::Chunk, storage: &'b mut Self::Storage, predicate: F);
    fn add_span_link<'b>(span: &'b mut Self::Span, storage: &mut Self::Storage) -> &'b mut Self::SpanLink;
    fn span_link_iterator(span: &'s Self::Span) -> std::slice::Iter<'s, Self::SpanLink>;
    fn retain_span_links<'b, F: FnMut(&mut Self::SpanLink, &mut Self::Storage) -> bool>(span: &'b mut Self::Span, storage: &'b mut Self::Storage, predicate: F);
    fn add_span_event<'b>(span: &'b mut Self::Span, storage: &mut Self::Storage) -> &'b mut Self::SpanEvent;
    fn span_event_iterator(span: &'s Self::Span) -> std::slice::Iter<'s, Self::SpanEvent>;
    fn retain_span_events<'b, F: FnMut(&mut Self::SpanEvent, &mut Self::Storage) -> bool>(span: &'b mut Self::Span, storage: &'b mut Self::Storage, predicate: F);

    fn get_trace_container_id(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_language_name(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_language_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_tracer_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_runtime_id(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_env(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_hostname(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_app_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;

    fn set_trace_container_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_language_name(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_language_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_tracer_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_runtime_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_env(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_hostname(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;
    fn set_trace_app_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text) where D: OwnedTraceData;

    fn get_chunk_priority<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> i32;
    fn get_chunk_origin(chunk: &'s Self::Chunk, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_chunk_dropped_trace<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> bool;
    fn get_chunk_trace_id<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> u128;
    fn get_chunk_sampling_mechanism<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> u32;

    fn set_chunk_priority(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: i32);
    fn set_chunk_origin(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: D::Text);
    fn set_chunk_dropped_trace(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: bool);
    fn set_chunk_trace_id(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u128) where D: OwnedTraceData;
    fn set_chunk_sampling_mechanism(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u32);

    fn get_span_service(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_name(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_resource(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_type(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_span_id(span: &'s Self::Span, storage: &'s Self::Storage) -> u64;
    fn get_span_parent_id(span: &'s Self::Span, storage: &'s Self::Storage) -> u64;
    fn get_span_start(span: &'s Self::Span, storage: &'s Self::Storage) -> i64;
    fn get_span_duration(span: &'s Self::Span, storage: &'s Self::Storage) -> i64;
    fn get_span_error(span: &'s Self::Span, storage: &'s Self::Storage) -> bool;
    fn get_span_env(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_version(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_component(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_kind(span: &'s Self::Span, storage: &'s Self::Storage) -> SpanKind;

    fn set_span_service(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_name(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_resource(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_type(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_span_id(span: &mut Self::Span, storage: &mut Self::Storage, value: u64);
    fn set_span_parent_id(span: &mut Self::Span, storage: &mut Self::Storage, value: u64);
    fn set_span_start(span: &mut Self::Span, storage: &mut Self::Storage, value: i64);
    fn set_span_duration(span: &mut Self::Span, storage: &mut Self::Storage, value: i64);
    fn set_span_error(span: &mut Self::Span, storage: &mut Self::Storage, value: bool);
    fn set_span_env(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_version(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_component(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_kind(span: &mut Self::Span, storage: &mut Self::Storage, value: SpanKind);

    fn get_link_trace_id(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u128;
    fn get_link_span_id(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u64;
    fn get_link_trace_state(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_link_flags(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u32;

    fn set_link_trace_id(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u128);
    fn set_link_span_id(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u64);
    fn set_link_trace_state(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: D::Text);
    fn set_link_flags(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u32);

    fn get_event_time_unix_nano(event: &'s Self::SpanEvent, storage: &'s Self::Storage) -> u64;
    fn get_event_name(event: &'s Self::SpanEvent, storage: &'s Self::Storage) -> &'s D::Text;

    fn set_event_time_unix_nano(event: &mut Self::SpanEvent, storage: &mut Self::Storage, value: u64);
    fn set_event_name(event: &mut Self::SpanEvent, storage: &mut Self::Storage, value: D::Text);
}
