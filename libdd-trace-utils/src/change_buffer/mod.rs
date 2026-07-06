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

        // Fetch the trace_id corresponding to this chunk. It must be the same for all the spans in
        // the chunk.
        let Some(fst_id) = span_ids.first() else {
            return Ok(vec![]);
        };

        let Some((_span, segment_id)) = self.spans.get(fst_id) else {
            return Err(ChangeBufferError::SpanNotFound(*fst_id));
        };

        let segment_id = *segment_id;
        let segment = self.segments.get(&segment_id);

        for span_id in span_ids {
            let (mut span, _segment_id) = self
                .spans
                .remove(span_id)
                .ok_or(ChangeBufferError::SpanNotFound(*span_id))?;

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
        let cached = match cache_slot.as_mut() {
            Some(cached) if op.span_id == cached.span_id => cached,
            _ => {
                let (span, segment_id) = self
                    .spans
                    .get_mut(&op.span_id)
                    .ok_or(ChangeBufferError::SpanNotFound(op.span_id))?;

                cache_slot.insert(SpanCache {
                    span_id: op.span_id,
                    // Safety: a mutable reference can't be null
                    // TODO: use NonNull::from_mut once our MRSV is recent enough
                    span_ptr: unsafe { NonNull::new_unchecked(span as *mut Span<T>) },
                    segment_id: *segment_id,
                })
            }
        };

        // Safety: span_ptr points into self.spans and is valid for write (safety pre-condition of
        // this function).
        // self.spans is never aliased/accessed otherwise for the lifetime of `span`.
        let span = unsafe { cached.span_ptr.as_mut() };

        match op.opcode {
            OpCode::SetMetaAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                span.meta.insert(key, val);
            }
            OpCode::SetMetricAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val: f64 = buf.read(index)?;
                span.metrics.insert(key, val);
            }
            OpCode::SetServiceName => {
                span.service = buf.read_string(&self.string_table, index)?;
            }
            OpCode::SetResourceName => {
                span.resource = buf.read_string(&self.string_table, index)?;
            }
            OpCode::SetError => {
                span.error = buf.read(index)?;
            }
            OpCode::SetStart => {
                span.start = buf.read(index)?;
            }
            OpCode::SetDuration => {
                span.duration = buf.read(index)?;
            }
            OpCode::SetType => {
                span.r#type = buf.read_string(&self.string_table, index)?;
            }
            OpCode::SetName => {
                span.name = buf.read_string(&self.string_table, index)?;
            }
            OpCode::SetTraceMetaAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;

                if let Some(segment) = self.segments.get_mut(&cached.segment_id) {
                    segment.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read(index)?;

                if let Some(segment) = self.segments.get_mut(&cached.segment_id) {
                    segment.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = buf.read_string(&self.string_table, index)?;

                if let Some(segment) = self.segments.get_mut(&cached.segment_id) {
                    segment.origin = Some(origin);
                }
            }
            OpCode::BatchSetMeta => {
                let count: u32 = buf.read(index)?;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val = buf.read_string(&self.string_table, index)?;
                    span.meta.insert(key, val);
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = buf.read(index)?;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val: f64 = buf.read(index)?;
                    span.metrics.insert(key, val);
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
}
