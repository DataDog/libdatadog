// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Change buffer.
//!
//! A change buffer is a contiguous shared memory area between libdatadog and an external runtime.
//! In order to amortize the cost of crossing the FFI when using native spans, the runtime writes
//! events into the change buffer instead of calling libdatadog many times, and only flushes by
//! batch — that flush is where the call to libdatadog happens. Libdatadog then processes the change
//! buffer and reconstructs the corresponding spans.
//!
//! The change buffer is currently designed and used for dd-trace-js, but the idea could be extended
//! to other runtime where the FFI cost is high.

/// Errors that can occur when operating on a [`ChangeBuffer`] or [`ChangeBufferState`].
#[derive(Debug)]
pub enum ChangeBufferError {
    SpanNotFound(u64),
    /// A string index didn't have any corresponding entry in the string table.
    StringNotFound(u32),
    /// A read is out of bounds.
    ReadOutOfBounds {
        /// The starting offset of the read.
        offset: usize,
        /// The size in bytes of the value attempted to be read starting at `offset`.
        /// We have `offset + value_len > buffer_len`.
        value_len: usize,
        /// The total size of the buffer.
        buffer_len: usize,
    },
    /// A is write is out of bounds.
    WriteOutOfBounds {
        /// The starting offset of the write.
        offset: usize,
        /// The size in bytes of the value attempted to be written starting at `offset`.
        /// We have `offset + value_len > buffer_len`.
        value_len: usize,
        /// The total size of the buffer.
        buffer_len: usize,
    },
    /// Unknown opcode.
    UnknownOpcode(u32),
}

impl std::fmt::Display for ChangeBufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeBufferError::SpanNotFound(id) => write!(f, "span not found: {id}"),
            ChangeBufferError::StringNotFound(id) => {
                write!(f, "string not found internally: {id}")
            }
            ChangeBufferError::ReadOutOfBounds {
                offset,
                value_len,
                buffer_len,
            } => {
                write!(f, "read out of bounds: offset={offset}, value_len={value_len}, buffer_len={buffer_len}")
            }
            ChangeBufferError::WriteOutOfBounds {
                offset,
                value_len,
                buffer_len,
            } => {
                write!(f, "write out of bounds: offset={offset}, value_len={value_len}, buffer_len={buffer_len}")
            }
            ChangeBufferError::UnknownOpcode(val) => write!(f, "unknown opcode: {val}"),
        }
    }
}

impl std::error::Error for ChangeBufferError {}

pub type Result<T> = std::result::Result<T, ChangeBufferError>;

mod utils;

mod segment;
pub use segment::{Segment, SmallSegmentMap};

mod operation;
use operation::*;

mod buffer;
pub use buffer::*;

use crate::span::v04::Span;
use crate::span::vec_map::VecMap;
use crate::span::{SpanText, TraceData};
use rustc_hash::FxHashMap;
use std::ptr::NonNull;

/// Interned string table (O(1) lookup vs HashMap).
///
/// Note: currently the string table never shrinks (it is never compacted). When entries are
/// evicted (freeing the backing strings), a small amount of memory is leaked (to hold the
/// `None` value).
pub(crate) struct StringTable<T>(Vec<Option<T>>);

impl<T: Clone> StringTable<T> {
    fn with_capacity(cap: usize) -> Self {
        Self(Vec::with_capacity(cap))
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub(crate) fn get(&self, id: u32) -> Option<T> {
        self.0.get(id as usize).and_then(|opt| opt.clone())
    }

    pub(crate) fn insert(&mut self, key: u32, val: T) {
        let idx = key as usize;
        if idx >= self.0.len() {
            self.0.resize_with(idx + 1, || None);
        }
        self.0[idx] = Some(val);
    }

    pub(crate) fn evict(&mut self, key: u32) {
        let idx = key as usize;
        if idx < self.0.len() {
            self.0[idx] = None;
        }
    }
}

/// A stateful wrapper around a change buffer for processing and span reconstructions.
pub struct ChangeBufferState<T: TraceData> {
    change_buffer: ChangeBuffer,
    /// Live spans, keyed by span_id. Each entry pairs the span with the segment_id
    /// assigned at Create time, co-locating the two pieces of data that are always
    /// looked up together.
    spans: FxHashMap<u64, (Span<T>, u64)>,
    segments: SmallSegmentMap<T::Text>,
    string_table: StringTable<T::Text>,
    tracer_service: T::Text,
    tracer_language: T::Text,
    pid: u32,
    /// Default meta tags automatically applied to every new span via create_span.
    default_meta: Vec<(T::Text, T::Text)>,
    // Cached static strings to avoid repeated heap allocations (e.g. Arc<str>) on every span
    // flush. These are created once and cloned (cheap ref bump).
    str_top_level: T::Text,
    str_measured: T::Text,
    str_base_service: T::Text,
    str_language: T::Text,
    str_process_id: T::Text,
    str_origin: T::Text,
    str_rule_psr: T::Text,
    str_limit_psr: T::Text,
    str_agent_psr: T::Text,
    str_internal: T::Text,
    /// Pool of recycled Span objects. Reusing spans (with their pre-allocated Vec buffers)
    /// eliminates the alloc/dealloc churn that fragments the WASM linear memory allocator over
    /// time.
    span_pool: Vec<Span<T>>,
    /// Count of operations (and chunk spans) skipped during a flush because their target span
    /// was not live in [`Self::spans`] — either already extracted (a late/duplicate op) or a
    /// Create that was never applied. Skipping keeps the flush resilient: one orphaned op no
    /// longer aborts the whole batch (which previously discarded every still-pending op,
    /// including unrelated Creates). Non-zero values are benign but worth surfacing.
    dropped_for_missing_span: u64,
}

fn new_span_pooled<T: TraceData>(
    pool: &mut Vec<Span<T>>,
    span_id: u64,
    parent_id: u64,
    trace_id: u128,
) -> Span<T> {
    if let Some(mut span) = pool.pop() {
        span.span_id = span_id;
        span.trace_id = trace_id;
        span.parent_id = parent_id;
        span.start = 0;
        span.duration = 0;
        span.error = 0;
        span.service = Default::default();
        span.name = Default::default();
        span.resource = Default::default();
        span.r#type = Default::default();
        span.meta.clear();
        span.metrics.clear();
        span.meta_struct.clear();
        span.span_links.clear();
        span.span_events.clear();
        span
    } else {
        Span {
            span_id,
            trace_id,
            parent_id,
            meta: VecMap::with_capacity(8),
            metrics: VecMap::with_capacity(4),
            ..Default::default()
        }
    }
}

// Similar to [ChangeBufferState::span_mut], but doesn't borrow the whole [ChangeBufferState].
fn span_at_mut<T: TraceData>(
    spans: &mut FxHashMap<u64, (Span<T>, u64)>,
    span_id: u64,
) -> Result<&mut Span<T>> {
    spans
        .get_mut(&span_id)
        .map(|(span, _segment_id)| span)
        .ok_or(ChangeBufferError::SpanNotFound(span_id))
}

/// Per-flush cache of the span in [ChangeBufferState::spans] for the most recently
/// processed span.
///
/// Avoids repeated lookups for consecutive ops on the same span. A [SpanCache] is invalidated
/// before any HashMap insertion that could trigger a rehash, that is before every Create op.
struct SpanCache<T: TraceData> {
    span_id: u64,
    span_ptr: NonNull<Span<T>>,
    segment_id: u64,
}

impl<T: TraceData> ChangeBufferState<T>
where
    T::Text: Clone,
{
    /// The maximun size of the recycled span pool, beyond which we don't recycle spans anymore but
    /// drop them.
    const SPANS_POOL_MAX_SIZE: usize = 128;
    /// Capacity for the initial allocation of the span table.
    const SPANS_CAPACITY: usize = 128;
    /// Capacity for the initial allocation of the string table.
    const STRING_TABLE_CAPACITY: usize = 128;

    pub fn new(
        change_buffer: ChangeBuffer,
        tracer_service: T::Text,
        tracer_language: T::Text,
        pid: u32,
    ) -> Self {
        ChangeBufferState {
            change_buffer,
            spans: FxHashMap::with_capacity_and_hasher(Self::SPANS_CAPACITY, Default::default()),
            segments: SmallSegmentMap::default(),
            string_table: StringTable::with_capacity(Self::STRING_TABLE_CAPACITY),
            tracer_service,
            tracer_language,
            pid,
            default_meta: Vec::new(),
            str_top_level: T::Text::from_static_str("_dd.top_level"),
            str_measured: T::Text::from_static_str("_dd.measured"),
            str_base_service: T::Text::from_static_str("_dd.base_service"),
            str_language: T::Text::from_static_str("language"),
            str_process_id: T::Text::from_static_str("process_id"),
            str_origin: T::Text::from_static_str("_dd.origin"),
            str_rule_psr: T::Text::from_static_str("_dd.rule_psr"),
            str_limit_psr: T::Text::from_static_str("_dd.limit_psr"),
            str_agent_psr: T::Text::from_static_str("_dd.agent_psr"),
            str_internal: T::Text::from_static_str("internal"),
            span_pool: Vec::new(),
            dropped_for_missing_span: 0,
        }
    }

    pub fn spans_count(&self) -> usize {
        self.spans.len()
    }

    pub fn string_table_len(&self) -> usize {
        self.string_table.len()
    }

    pub fn span_pool_len(&self) -> usize {
        self.span_pool.len()
    }

    /// Number of operations/spans skipped during flushes because their target span was not
    /// live (already extracted, or a Create that was never applied). Non-zero values are
    /// benign — they indicate late or duplicate operations rather than a fault — but callers
    /// may surface this as a metric to monitor pipeline health.
    pub fn dropped_for_missing_span(&self) -> u64 {
        self.dropped_for_missing_span
    }

    pub fn recycle_spans(&mut self, spans: Vec<Span<T>>) {
        let available = Self::SPANS_POOL_MAX_SIZE.saturating_sub(self.span_pool.len());
        for span in spans.into_iter().take(available) {
            self.span_pool.push(span);
        }
    }

    pub fn flush_chunk(
        &mut self,
        span_ids: &[u64],
        first_is_local_root: bool,
    ) -> Result<Vec<Span<T>>> {
        let mut is_local_root = first_is_local_root;
        let mut is_chunk_root = true;

        let mut spans_vec = Vec::with_capacity(span_ids.len());

        // The segment (and trace_id) is the same for all spans in the chunk. Establish it from
        // the first span that is actually live: the nominal first id may be absent (never
        // created, or already extracted), so fall back to the first present id rather than
        // aborting the whole chunk. If none are live there is nothing to extract.
        let Some(segment_id) = span_ids
            .iter()
            .find_map(|id| self.spans.get(id).map(|(_, segment_id)| *segment_id))
        else {
            return Ok(vec![]);
        };

        let segment = self.segments.get(&segment_id);
        let mut skipped: u64 = 0;

        for span_id in span_ids {
            let Some((mut span, _segment_id)) = self.spans.remove(span_id) else {
                // Span isn't live (already extracted, or its Create was never applied). Skip
                // it instead of aborting the chunk so the remaining spans still export.
                skipped += 1;
                tracing::debug!(
                    span_id,
                    "change_buffer: skipping chunk span not present in the map"
                );
                continue;
            };

            if is_local_root {
                self.copy_in_sampling_tags(segment, &mut span);
                span.metrics.insert(self.str_top_level.clone(), 1.0);
                is_local_root = false;
            }

            if is_chunk_root {
                Self::copy_in_chunk_tags(segment, &mut span);
                is_chunk_root = false;
            }

            self.process_span(segment, &mut span);
            spans_vec.push(span);
        }

        self.dropped_for_missing_span += skipped;

        let segment = self.segments.get_mut(&segment_id);

        let should_remove = segment
            .map(|segment| {
                if segment.span_count <= spans_vec.len() {
                    true
                } else {
                    segment.span_count -= spans_vec.len();
                    false
                }
            })
            .unwrap_or(false);

        if should_remove {
            self.segments.remove(&segment_id);
        }

        Ok(spans_vec)
    }

    fn copy_in_sampling_tags(&self, segment: Option<&Segment<T::Text>>, span: &mut Span<T>) {
        if let Some(segment) = segment {
            if let Some(rule) = segment.sampling_rule_decision {
                span.metrics.insert(self.str_rule_psr.clone(), rule);
            }
            if let Some(rule) = segment.sampling_limit_decision {
                span.metrics.insert(self.str_limit_psr.clone(), rule);
            }
            if let Some(rule) = segment.sampling_agent_decision {
                span.metrics.insert(self.str_agent_psr.clone(), rule);
            }
        }
    }

    fn copy_in_chunk_tags(segment: Option<&Segment<T::Text>>, span: &mut Span<T>) {
        if let Some(segment) = segment {
            span.meta
                .extend(segment.meta.iter().map(|(k, v)| (k.clone(), v.clone())));
            span.metrics
                .extend(segment.metrics.iter().map(|(k, v)| (k.clone(), *v)));
        }
    }

    fn process_span(&self, segment: Option<&Segment<T::Text>>, span: &mut Span<T>) {
        if let Some(kind) = span.meta.get("kind") {
            if *kind != self.str_internal {
                span.metrics.insert(self.str_measured.clone(), 1.0);
            }
        }

        if span.service != self.tracer_service {
            span.meta
                .insert(self.str_base_service.clone(), self.tracer_service.clone());
        }

        span.meta
            .insert(self.str_language.clone(), self.tracer_language.clone());
        span.metrics
            .insert(self.str_process_id.clone(), f64::from(self.pid));

        if let Some(segment) = segment {
            if let Some(origin) = segment.origin.clone() {
                span.meta.insert(self.str_origin.clone(), origin);
            }
        }
    }

    pub fn flush_change_buffer(&mut self) -> Result<()> {
        let mut index = 0;
        let mut count = self.change_buffer.read::<u64>(&mut index)? as u32;

        // Cached span_id and pointer to a span in the `span` HashMap.
        //
        // When applying span operations, we cache the last span_id and direct pointer to its entry
        // in `spans`. This saves repeated HashMap lookups for consecutive ops targeting the same
        // span.
        let mut cache = None;

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index)?;

            match op.opcode {
                OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => {
                    cache = None;
                    self.interpret_operation(&mut index, &op)?;
                }
                _ => {
                    // Safety: the pointer is valid as long as no new keys are inserted in the
                    // HashMap, which only happens in Create ops. Create ops reset the cache (set
                    // pointers to null) before inserting, so by the time we use a cached pointer
                    // again, no rehash has occurred since it was obtained.
                    unsafe {
                        self.interpret_operation_cached(&mut index, &op, &mut cache)?;
                    }
                }
            }
            count -= 1;
        }

        self.change_buffer.write_u32(0, 0)?;
        self.change_buffer.write_u32(4, 0)?;

        Ok(())
    }

    /// This method doesn't support [OpCode::Create], [OpCode::CreateSpan] nor
    /// [OpCode::CreateSpanFull]. To avoid panicking, we return [ChangeBufferError::UnknownOpcode]
    /// with the special value `u32::MAX` in release mode in that case, but this shouldn't happen
    /// and is a logical/internal error.
    ///
    /// # Safety
    ///
    /// `cache.span_ptr` must be a pointer valid for writes into `self.spans`. This method
    /// guarantees that it remains valid (it doesn't cause `self.spans` to invalidate the pointer,
    /// e.g. by causing re-allocation).
    unsafe fn interpret_operation_cached(
        &mut self,
        index: &mut usize,
        op: &BufferedOperation,
        cache_slot: &mut Option<SpanCache<T>>,
    ) -> Result<()> {
        let buf = &self.change_buffer;
        // Resolve the target span. It may be absent if it was already extracted (a late or
        // duplicate op referencing an exported span) or if its Create was never applied. In
        // that case we DO NOT abort: we still parse the op's payload below (advancing `index`)
        // so the read cursor stays aligned for the remaining ops, but skip the mutation. This
        // keeps a single orphaned op from discarding every still-pending op in the batch.
        let cached: Option<&mut SpanCache<T>> = match cache_slot.as_mut() {
            Some(cached) if op.span_id == cached.span_id => Some(cached),
            _ => match self.spans.get_mut(&op.span_id) {
                Some((span, segment_id)) => Some(cache_slot.insert(SpanCache {
                    span_id: op.span_id,
                    // Safety: a mutable reference can't be null
                    // TODO: use NonNull::from_mut once our MRSV is recent enough
                    span_ptr: unsafe { NonNull::new_unchecked(span as *mut Span<T>) },
                    segment_id: *segment_id,
                })),
                None => {
                    // Don't cache a miss; drop any stale cache from a prior span.
                    *cache_slot = None;
                    self.dropped_for_missing_span += 1;
                    tracing::debug!(
                        span_id = op.span_id,
                        "change_buffer: skipping op for span not present in the map"
                    );
                    None
                }
            },
        };

        let segment_id = cached.as_ref().map_or(0, |c| c.segment_id);
        // Safety: span_ptr points into self.spans and is valid for write (safety pre-condition of
        // this function).
        // self.spans is never aliased/accessed otherwise for the lifetime of `span`.
        let span = cached.map(|c| unsafe { c.span_ptr.as_mut() });

        match op.opcode {
            OpCode::SetMetaAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                if let Some(span) = span {
                    span.meta.insert(key, val);
                }
            }
            OpCode::SetMetricAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val: f64 = buf.read(index)?;
                if let Some(span) = span {
                    span.metrics.insert(key, val);
                }
            }
            OpCode::SetServiceName => {
                let service = buf.read_string(&self.string_table, index)?;
                if let Some(span) = span {
                    span.service = service;
                }
            }
            OpCode::SetResourceName => {
                let resource = buf.read_string(&self.string_table, index)?;
                if let Some(span) = span {
                    span.resource = resource;
                }
            }
            OpCode::SetError => {
                let error = buf.read(index)?;
                if let Some(span) = span {
                    span.error = error;
                }
            }
            OpCode::SetStart => {
                let start = buf.read(index)?;
                if let Some(span) = span {
                    span.start = start;
                }
            }
            OpCode::SetDuration => {
                let duration = buf.read(index)?;
                if let Some(span) = span {
                    span.duration = duration;
                }
            }
            OpCode::SetType => {
                let r#type = buf.read_string(&self.string_table, index)?;
                if let Some(span) = span {
                    span.r#type = r#type;
                }
            }
            OpCode::SetName => {
                let name = buf.read_string(&self.string_table, index)?;
                if let Some(span) = span {
                    span.name = name;
                }
            }
            OpCode::SetTraceMetaAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;

                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read(index)?;

                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = buf.read_string(&self.string_table, index)?;

                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.origin = Some(origin);
                }
            }
            OpCode::BatchSetMeta => {
                let count: u32 = buf.read(index)?;
                let mut span = span;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val = buf.read_string(&self.string_table, index)?;
                    if let Some(span) = span.as_deref_mut() {
                        span.meta.insert(key, val);
                    }
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = buf.read(index)?;
                let mut span = span;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val: f64 = buf.read(index)?;
                    if let Some(span) = span.as_deref_mut() {
                        span.metrics.insert(key, val);
                    }
                }
            }
            OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => {
                debug_assert!(false, "didn't expect Create, CreateSpan or CreateSpanFull in interpret_operation_cached");
                return Err(ChangeBufferError::UnknownOpcode(u32::MAX));
            }
        }

        Ok(())
    }

    #[inline]
    pub fn get_span(&self, span_id: u64) -> Result<&Span<T>> {
        self.spans
            .get(&span_id)
            .map(|(span, _)| span)
            .ok_or(ChangeBufferError::SpanNotFound(span_id))
    }

    #[inline]
    pub fn get_segment(&self, id: &u64) -> Option<&Segment<T::Text>> {
        self.segments.get(id)
    }

    #[inline]
    pub fn span_mut(&mut self, span_id: u64) -> Result<&mut Span<T>> {
        span_at_mut(&mut self.spans, span_id)
    }

    #[inline]
    pub fn set_default_meta(&mut self, tags: Vec<(T::Text, T::Text)>) {
        self.default_meta = tags;
    }

    fn insert_span(&mut self, span_id: u64, segment_id: u64, span: Span<T>) {
        self.spans.insert(span_id, (span, segment_id));
        self.segments.get_or_insert_default(segment_id).span_count += 1;
    }

    fn apply_default_meta(&self, span: &mut Span<T>) {
        for (key, value) in &self.default_meta {
            span.meta.insert(key.clone(), value.clone());
        }
    }

    fn interpret_operation(&mut self, index: &mut usize, op: &BufferedOperation) -> Result<()> {
        let buf = &self.change_buffer;

        match op.opcode {
            OpCode::Create => {
                let trace_id: u128 = self.change_buffer.read(index)?;
                let segment_id: u64 = buf.read(index)?;
                let parent_id = buf.read(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, op.span_id, parent_id, trace_id);
                self.apply_default_meta(&mut span);
                self.insert_span(op.span_id, segment_id, span);
            }
            OpCode::SetMetaAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, op.span_id)?
                    .meta
                    .insert(key, val);
            }
            OpCode::SetMetricAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val: f64 = buf.read(index)?;
                span_at_mut(&mut self.spans, op.span_id)?
                    .metrics
                    .insert(key, val);
            }
            OpCode::SetServiceName => {
                let service = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, op.span_id)?.service = service;
            }
            OpCode::SetResourceName => {
                let resource = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, op.span_id)?.resource = resource;
            }
            OpCode::SetError => {
                let error = buf.read(index)?;
                span_at_mut(&mut self.spans, op.span_id)?.error = error;
            }
            OpCode::SetStart => {
                let start = buf.read(index)?;
                span_at_mut(&mut self.spans, op.span_id)?.start = start;
            }
            OpCode::SetDuration => {
                let duration = buf.read(index)?;
                span_at_mut(&mut self.spans, op.span_id)?.duration = duration;
            }
            OpCode::SetType => {
                let r#type = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, op.span_id)?.r#type = r#type;
            }
            OpCode::SetName => {
                let name = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, op.span_id)?.name = name;
            }
            OpCode::SetTraceMetaAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                let segment_id = self.spans.get(&op.span_id).map(|(_, id)| *id).unwrap_or(0);
                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read(index)?;
                let segment_id = self.spans.get(&op.span_id).map(|(_, id)| *id).unwrap_or(0);
                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = buf.read_string(&self.string_table, index)?;
                let segment_id = self.spans.get(&op.span_id).map(|(_, id)| *id).unwrap_or(0);
                if let Some(segment) = self.segments.get_mut(&segment_id) {
                    segment.origin = Some(origin);
                }
            }
            OpCode::CreateSpan => {
                let trace_id: u128 = buf.read(index)?;
                let segment_id: u64 = buf.read(index)?;
                let parent_id: u64 = buf.read(index)?;
                let name = buf.read_string(&self.string_table, index)?;
                let start: i64 = buf.read(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, op.span_id, parent_id, trace_id);
                span.name = name;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.insert_span(op.span_id, segment_id, span);
            }
            OpCode::CreateSpanFull => {
                let trace_id: u128 = buf.read(index)?;
                let segment_id: u64 = buf.read(index)?;
                let parent_id: u64 = buf.read(index)?;
                let name = buf.read_string(&self.string_table, index)?;
                let service = buf.read_string(&self.string_table, index)?;
                let resource = buf.read_string(&self.string_table, index)?;
                let r#type = buf.read_string(&self.string_table, index)?;
                let start: i64 = buf.read(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, op.span_id, parent_id, trace_id);
                span.name = name;
                span.service = service;
                span.resource = resource;
                span.r#type = r#type;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.insert_span(op.span_id, segment_id, span);
            }
            OpCode::BatchSetMeta => {
                let count: u32 = buf.read(index)?;
                let span = span_at_mut(&mut self.spans, op.span_id)?;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val = buf.read_string(&self.string_table, index)?;
                    span.meta.insert(key, val);
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = buf.read(index)?;
                let span = span_at_mut(&mut self.spans, op.span_id)?;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val: f64 = buf.read(index)?;
                    span.metrics.insert(key, val);
                }
            }
        };

        Ok(())
    }

    #[inline]
    pub fn string_table_insert_one(&mut self, key: u32, val: T::Text) {
        self.string_table.insert(key, val);
    }

    #[inline]
    pub fn string_table_evict_one(&mut self, key: u32) {
        self.string_table.evict(key);
    }
}

/// Tests for segment isolation when the same trace ID appears in two
/// independent segments (e.g. service A → service B → service A).
///
/// The scenario: a single Node.js tracer processes two separate "chunks" that
/// share a trace ID — the first visit by service A and the re-entry by
/// service A after service B calls back.  Trace-level operations (origin,
/// meta, metrics) written for the second segment must not bleed into the
/// first segment when it is flushed.
///
/// These tests document the correct behavior and pass once each `flush_chunk`
/// call operates on its own isolated segment state (keyed by `segment_id`
/// rather than `trace_id`).
#[cfg(test)]
mod segment_isolation_tests {
    use super::*;
    use crate::span::SliceData;

    // -----------------------------------------------------------------------
    // Minimal builder for the raw change-buffer byte format.
    // -----------------------------------------------------------------------

    struct BufWriter {
        data: Vec<u8>,
        count: u64,
    }

    impl BufWriter {
        fn new() -> Self {
            let mut data = Vec::with_capacity(256);
            // Reserve 8 bytes for the operation-count header (filled in by finish()).
            data.extend_from_slice(&0u64.to_le_bytes());
            BufWriter { data, count: 0 }
        }

        fn u32(&mut self, v: u32) {
            self.data.extend_from_slice(&v.to_le_bytes());
        }
        fn u64(&mut self, v: u64) {
            self.data.extend_from_slice(&v.to_le_bytes());
        }
        fn u128(&mut self, v: u128) {
            self.data.extend_from_slice(&v.to_le_bytes());
        }

        // Write an operation header (opcode as u16 + span_id as u64) and bump count.
        fn op(&mut self, opcode: u16, span_id: u64) {
            self.data.extend_from_slice(&opcode.to_le_bytes());
            self.u64(span_id);
            self.count += 1;
        }

        fn finish(mut self) -> Vec<u8> {
            self.data[0..8].copy_from_slice(&self.count.to_le_bytes());
            self.data
        }
    }

    // Construct a ChangeBufferState backed by the provided bytes.
    //
    // # Safety
    // The returned state borrows `buf_data` via a raw pointer; the caller must
    // keep `buf_data` alive and unmodified for as long as the state is used.
    fn make_state(buf_data: &mut Vec<u8>) -> ChangeBufferState<SliceData<'static>> {
        // SAFETY: buf_data is pre-allocated to its final size before this call,
        // so no reallocation will occur while the ChangeBuffer is alive.
        let cb = unsafe {
            ChangeBuffer::from_raw_parts(
                NonNull::new(buf_data.as_mut_ptr()).unwrap(),
                buf_data.len(),
            )
        };
        ChangeBufferState::new(cb, "test-service", "javascript", 1)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    // Opcode constants (mirror OpCode enum values).
    const OP_CREATE: u16 = 0;
    const OP_SET_SERVICE_NAME: u16 = 3;
    const OP_SET_TRACE_ORIGIN: u16 = 12;
    const OP_SET_TRACE_META_ATTR: u16 = 10;
    const OP_SET_TRACE_METRICS_ATTR: u16 = 11;
    const OP_BATCH_SET_META: u16 = 15;
    const OP_BATCH_SET_METRIC: u16 = 16;

    const TRACE_ID: u128 = 0xABCD;

    #[test]
    fn set_trace_origin_on_second_segment_does_not_affect_first_segment() {
        let mut w = BufWriter::new();

        // Segment 1 — span_id=1: origin → "origin-A" (string id 0)
        w.op(OP_CREATE, 1); // span_id=1 in header
        w.u128(TRACE_ID);
        w.u64(1); // segment_id=1
        w.u64(0); // parent_id
        w.op(OP_SET_TRACE_ORIGIN, 1);
        w.u32(0); // string_id → "origin-A"

        // Segment 2 — span_id=2, same trace but different segment
        w.op(OP_CREATE, 2); // span_id=2 in header
        w.u128(TRACE_ID);
        w.u64(2); // segment_id=2
        w.u64(1); // parent_id
        w.op(OP_SET_TRACE_ORIGIN, 2);
        w.u32(1); // string_id → "origin-B"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "origin-A");
        state.string_table_insert_one(1, "origin-B");

        state.flush_change_buffer().unwrap();

        // Flush only segment 1.
        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);

        // Segment 1 must carry its own origin, not segment 2's.
        assert_eq!(
            spans[0].meta.get("_dd.origin"),
            Some(&"origin-A"),
            "segment 1 origin was overwritten by segment 2's SetTraceOrigin"
        );
    }

    #[test]
    fn set_trace_meta_on_second_segment_does_not_affect_first_segment() {
        let mut w = BufWriter::new();

        // Segment 1 — span_id=1: trace meta env=production
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1); // segment_id=1
        w.u64(0);
        w.op(OP_SET_TRACE_META_ATTR, 1);
        w.u32(0); // key   → "env"
        w.u32(1); // value → "production"

        // Segment 2 — span_id=2, same trace but different segment
        w.op(OP_CREATE, 2);
        w.u128(TRACE_ID);
        w.u64(2); // segment_id=2
        w.u64(1);
        w.op(OP_SET_TRACE_META_ATTR, 2);
        w.u32(0); // key   → "env"
        w.u32(2); // value → "staging"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "env");
        state.string_table_insert_one(1, "production");
        state.string_table_insert_one(2, "staging");

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);

        // Segment 1's chunk root must not carry segment 2's value.
        assert_eq!(
            spans[0].meta.get("env"),
            Some(&"production"),
            "segment 1 trace meta was polluted by segment 2's SetTraceMetaAttr"
        );
    }

    // Note: now deferred data don't exist anymore, but we keep the test nontheless.
    //
    // Previously: a regression test for P1 buffer-corruption bug: when `cached_deferred_meta`
    // is null (because `materialize_span` drained it between flushes), a
    // `BatchSetMeta` op in the cached path must still consume all its payload
    // bytes so that the ops that follow are decoded from the correct position.
    #[test]
    fn batch_set_meta_after_materialize_span_consumes_payload_bytes() {
        const SPAN_A: u64 = 1;
        const SPAN_B: u64 = 2;

        // First buffer: just create span A.
        let mut w1 = BufWriter::new();
        w1.op(OP_CREATE, SPAN_A);
        w1.u128(TRACE_ID);
        w1.u64(1); // segment_id
        w1.u64(0); // parent_id
        let first_buf = w1.finish();

        // Second buffer: Create span B, then BatchSetMeta for span A (1 pair),
        // then SetServiceName for span B.
        // String table: 0="key", 1="val", 2="service-B"
        let mut w2 = BufWriter::new();
        w2.op(OP_CREATE, SPAN_B);
        w2.u128(TRACE_ID);
        w2.u64(2); // segment_id
        w2.u64(SPAN_A); // parent_id
        w2.op(OP_BATCH_SET_META, SPAN_A);
        w2.u32(1); // count
        w2.u32(0); // key_id
        w2.u32(1); // val_id
        w2.op(OP_SET_SERVICE_NAME, SPAN_B);
        w2.u32(2); // string_id → "service-B"
        let second_buf = w2.finish();

        // Pre-allocate buf_data large enough for both buffers.
        let capacity = first_buf.len().max(second_buf.len()) + 16;
        let mut buf_data = vec![0u8; capacity];
        buf_data[..first_buf.len()].copy_from_slice(&first_buf);

        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "key");
        state.string_table_insert_one(1, "val");
        state.string_table_insert_one(2, "service-B");

        // First flush: creates span A.
        state.flush_change_buffer().unwrap();

        // Write second buffer into buf_data in-place (the ChangeBuffer raw pointer
        // still points at buf_data, so flush_change_buffer will see these new bytes).
        buf_data[..second_buf.len()].copy_from_slice(&second_buf);

        // Second flush: Create B (resets cache), BatchSetMeta for A (cache is None), SetServiceName
        // for B. Without the fix, SetServiceName reads from the BatchSetMeta payload bytes and gets
        // the wrong string id.
        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[SPAN_B], false).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "service-B",
            "SetServiceName decoded wrong bytes because BatchSetMeta left its \
             payload unread when deferred_meta was null"
        );
    }

    // Same as above but for BatchSetMetric.
    #[test]
    fn batch_set_metric_after_materialize_span_consumes_payload_bytes() {
        const SPAN_A: u64 = 1;
        const SPAN_B: u64 = 2;

        let mut w1 = BufWriter::new();
        w1.op(OP_CREATE, SPAN_A);
        w1.u128(TRACE_ID);
        w1.u64(1);
        w1.u64(0);
        let first_buf = w1.finish();

        let mut w2 = BufWriter::new();
        w2.op(OP_CREATE, SPAN_B);
        w2.u128(TRACE_ID);
        w2.u64(2);
        w2.u64(SPAN_A);
        w2.op(OP_BATCH_SET_METRIC, SPAN_A);
        w2.u32(1); // count
        w2.u32(0); // key_id
        w2.u64(1.5f64.to_bits()); // value
        w2.op(OP_SET_SERVICE_NAME, SPAN_B);
        w2.u32(2); // string_id → "service-B"
        let second_buf = w2.finish();

        let capacity = first_buf.len().max(second_buf.len()) + 16;
        let mut buf_data = vec![0u8; capacity];
        buf_data[..first_buf.len()].copy_from_slice(&first_buf);

        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "key");
        state.string_table_insert_one(2, "service-B");

        state.flush_change_buffer().unwrap();

        buf_data[..second_buf.len()].copy_from_slice(&second_buf);

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[SPAN_B], false).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "service-B",
            "SetServiceName decoded wrong bytes because BatchSetMetric left its \
             payload unread when deferred_metrics was null"
        );
    }

    #[test]
    fn set_trace_metrics_on_second_segment_does_not_affect_first_segment() {
        let mut w = BufWriter::new();

        // Segment 1 — span_id=1: trace metric "priority"=1.0
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1); // segment_id=1
        w.u64(0);
        w.op(OP_SET_TRACE_METRICS_ATTR, 1);
        w.u32(0); // key   → "priority"
        w.u64(1.0f64.to_bits()); // value → 1.0

        // Segment 2 — span_id=2, same trace but different segment
        w.op(OP_CREATE, 2);
        w.u128(TRACE_ID);
        w.u64(2); // segment_id=2
        w.u64(1);
        w.op(OP_SET_TRACE_METRICS_ATTR, 2);
        w.u32(0); // key   → "priority"
        w.u64(2.0f64.to_bits()); // value → 2.0

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "priority");

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);

        assert_eq!(
            spans[0].metrics.get("priority"),
            Some(&1.0f64),
            "segment 1 trace metric was polluted by segment 2's SetTraceMetricsAttr"
        );
    }

    // -----------------------------------------------------------------------
    // Tolerance for operations/spans whose target span is not live.
    //
    // A span can be absent from the map because it was already extracted (a
    // late or duplicate op for an exported span) or because its Create was
    // never applied. Such ops/spans must be skipped — not treated as a hard
    // error that aborts the whole flush/chunk — while keeping the buffer read
    // cursor aligned so subsequent ops decode correctly.
    // -----------------------------------------------------------------------

    const OP_MISSING: u64 = 99; // a span id that is never created

    #[test]
    fn op_for_missing_span_is_skipped_and_keeps_cursor_aligned() {
        // Create span 1, then a SetServiceName for a missing span (skipped),
        // then a SetServiceName for span 1. If the missing op's payload were
        // not consumed, span 1's service would decode from the wrong bytes.
        let mut w = BufWriter::new();
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1); // segment_id
        w.u64(0); // parent_id
        w.op(OP_SET_SERVICE_NAME, OP_MISSING);
        w.u32(0); // string_id → "svc-missing"
        w.op(OP_SET_SERVICE_NAME, 1);
        w.u32(1); // string_id → "svc-live"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "svc-missing");
        state.string_table_insert_one(1, "svc-live");

        // Must not error even though span 99 is absent.
        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "svc-live",
            "op after a skipped missing-span op decoded the wrong bytes"
        );
        assert_eq!(state.dropped_for_missing_span(), 1);
    }

    #[test]
    fn missing_span_op_does_not_abort_pending_creates() {
        // Regression for the cascade: an op for a missing span appears before a
        // Create in the same batch. Aborting on the missing op (old behavior)
        // dropped the still-pending Create, orphaning that span at export time.
        let mut w = BufWriter::new();
        w.op(OP_SET_SERVICE_NAME, OP_MISSING);
        w.u32(0); // string_id → "svc-missing"
        w.op(OP_CREATE, 2);
        w.u128(TRACE_ID);
        w.u64(1); // segment_id
        w.u64(0); // parent_id

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "svc-missing");

        state.flush_change_buffer().unwrap();

        // Span 2's Create must have landed despite the earlier missing-span op.
        let spans = state.flush_chunk(&[2], true).unwrap();
        assert_eq!(spans.len(), 1, "Create was dropped by the missing-span op");
        assert_eq!(state.dropped_for_missing_span(), 1);
    }

    #[test]
    fn flush_chunk_skips_missing_spans() {
        // A chunk listing a mix of live and absent span ids extracts only the
        // live ones instead of aborting the whole chunk.
        let mut w = BufWriter::new();
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1);
        w.u64(0);
        w.op(OP_CREATE, 2);
        w.u128(TRACE_ID);
        w.u64(1);
        w.u64(1);

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1, OP_MISSING, 2], true).unwrap();
        assert_eq!(spans.len(), 2, "missing chunk span aborted the extract");
        assert_eq!(state.dropped_for_missing_span(), 1);
    }

    #[test]
    fn flush_chunk_all_missing_is_empty_not_error() {
        // A chunk whose spans are all absent yields an empty result, not an error.
        let w = BufWriter::new();
        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[OP_MISSING], true).unwrap();
        assert!(spans.is_empty());
    }

    #[test]
    fn batch_op_for_missing_span_consumes_payload() {
        // A BatchSetMeta for a missing span must consume its full variable-length
        // payload (count + pairs) so the following op decodes from the right offset.
        let mut w = BufWriter::new();
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1);
        w.u64(0);
        w.op(OP_BATCH_SET_META, OP_MISSING);
        w.u32(2); // count
        w.u32(0); // key_id
        w.u32(0); // val_id
        w.u32(0); // key_id
        w.u32(0); // val_id
        w.op(OP_SET_SERVICE_NAME, 1);
        w.u32(1); // string_id → "svc-live"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "k");
        state.string_table_insert_one(1, "svc-live");

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "svc-live",
            "SetServiceName decoded wrong bytes: missing-span BatchSetMeta payload not consumed"
        );
        assert_eq!(state.dropped_for_missing_span(), 1);
    }

    #[test]
    fn batch_metric_op_for_missing_span_consumes_payload() {
        // Symmetric to the BatchSetMeta case: a BatchSetMetric for a missing span must
        // consume its full count-prefixed payload so the following op stays aligned.
        let mut w = BufWriter::new();
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1);
        w.u64(0);
        w.op(OP_BATCH_SET_METRIC, OP_MISSING);
        w.u32(2); // count
        w.u32(0); // key_id
        w.u64(1.5f64.to_bits()); // value
        w.u32(0); // key_id
        w.u64(2.5f64.to_bits()); // value
        w.op(OP_SET_SERVICE_NAME, 1);
        w.u32(1); // string_id → "svc-live"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "k");
        state.string_table_insert_one(1, "svc-live");

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "svc-live",
            "SetServiceName decoded wrong bytes: missing-span BatchSetMetric payload not consumed"
        );
        assert_eq!(state.dropped_for_missing_span(), 1);
    }

    #[test]
    fn trace_level_op_for_missing_span_is_skipped_and_keeps_cursor_aligned() {
        // A trace-level op (SetTraceMetaAttr) targeting a missing span must consume its
        // payload (keeping the following op aligned) and must not pollute a live segment:
        // the absent span resolves segment_id to the 0 sentinel, which matches no live
        // segment.
        let mut w = BufWriter::new();
        w.op(OP_CREATE, 1);
        w.u128(TRACE_ID);
        w.u64(1); // segment_id
        w.u64(0);
        w.op(OP_SET_TRACE_META_ATTR, OP_MISSING);
        w.u32(0); // key   → "env"
        w.u32(1); // value → "staging"
        w.op(OP_SET_SERVICE_NAME, 1);
        w.u32(2); // string_id → "svc-live"

        let mut buf_data = w.finish();
        let mut state = make_state(&mut buf_data);
        state.string_table_insert_one(0, "env");
        state.string_table_insert_one(1, "staging");
        state.string_table_insert_one(2, "svc-live");

        state.flush_change_buffer().unwrap();

        let spans = state.flush_chunk(&[1], true).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].service, "svc-live",
            "cursor misaligned after a skipped trace-level op"
        );
        assert_eq!(
            spans[0].meta.get("env"),
            None,
            "live segment polluted by a trace-level op for a missing span"
        );
        assert_eq!(state.dropped_for_missing_span(), 1);
    }
}
